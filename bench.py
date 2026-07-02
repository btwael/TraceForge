#!/usr/bin/env python3
import argparse
import fnmatch
import os
import re
import shutil
import signal
import subprocess
import sys
import threading
import time
from concurrent.futures import FIRST_COMPLETED, ThreadPoolExecutor, wait
from dataclasses import dataclass
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError as exc:
    raise SystemExit("Python 3.11+ is required") from exc

CONFIG = "bench.toml"
MODE_VALUES = {"full", "rounds", "dpor"}
CORE_KEYS = ("rayon", "parallel", "workers")
RUN_RESERVED = {"protocol", "modes"}
ACTIVE_PROCS = {}
ACTIVE_PROCS_LOCK = threading.Lock()
TERMINATING = False
TERMINATION_SIGNAL = None


@dataclass(frozen=True)
class Job:
    case_id: str
    protocol: str
    binary: str
    mode: str
    args: tuple[str, ...]
    cores: int
    params: dict


def slug(value):
    return re.sub(r"[^A-Za-z0-9._-]+", "-", str(value)).strip("-")


def toml_quote(value):
    escaped = str(value).replace("\\", "\\\\").replace('"', '\\"')
    return f'"{escaped}"'


def load_config(path):
    with Path(path).open("rb") as f:
        return tomllib.load(f)


def settings(config, args):
    raw = config.get("settings", {})
    available_cores = args.available_cores or int(raw.get("available_cores", os.cpu_count() or 1))
    jobs = args.jobs or int(raw.get("jobs", available_cores))
    timeout_hours = args.timeout_hours
    if timeout_hours is None:
        timeout_hours = float(raw.get("timeout_hours", 1))
    snapshot_interval_hours = args.snapshot_interval_hours
    if snapshot_interval_hours is None:
        snapshot_interval_hours = float(raw.get("snapshot_interval_hours", 1))
    bin_dir = Path(args.bin_dir or raw.get("bin_dir", "./target/release"))
    results_dir = Path(args.results_dir or raw.get("results_dir", "results"))
    return {
        "available_cores": available_cores,
        "jobs": jobs,
        "timeout_hours": timeout_hours,
        "timeout_seconds": timeout_hours * 3600,
        "snapshot_interval_hours": snapshot_interval_hours,
        "snapshot_interval_seconds": snapshot_interval_hours * 3600,
        "bin_dir": bin_dir,
        "results_dir": results_dir,
    }


def protocol_table(config):
    protocols = config.get("protocol")
    if not isinstance(protocols, dict):
        raise ValueError("bench.toml must define [protocol.<name>] tables")
    return protocols


def normalize_mode(mode):
    if mode not in MODE_VALUES:
        raise ValueError(f"invalid mode {mode!r}; expected one of {sorted(MODE_VALUES)}")
    return mode


def param_items(params):
    return [(key, params[key]) for key in sorted(params)]


def case_id(protocol, params, mode):
    parts = [protocol]
    for key, value in param_items(params):
        if isinstance(value, bool):
            if value:
                parts.append(key.replace("_", "-"))
            continue
        parts.append(f"{key.replace('_', '-')}-{value}")
    parts.append(f"mode-{mode}")
    return "__".join(slug(part) for part in parts if slug(part))


def core_cost(params):
    present = [key for key in CORE_KEYS if key in params]
    if len(present) > 1:
        raise ValueError(f"only one core-consuming key is allowed, got {present}")
    if not present:
        return 1
    value = int(params[present[0]])
    if value < 1:
        raise ValueError(f"{present[0]} must be >= 1")
    return value


def flag_args(params):
    args = []
    for key, value in param_items(params):
        flag = "--" + key.replace("_", "-")
        if isinstance(value, bool):
            if value:
                args.append(flag)
            continue
        args.extend([flag, str(value)])
    return args


def build_job(protocols, bin_dir, run, mode):
    protocol = run.get("protocol")
    if protocol not in protocols:
        raise ValueError(f"unknown protocol {protocol!r}")
    binary = protocols[protocol].get("binary")
    if not binary:
        raise ValueError(f"protocol {protocol!r} must define binary")
    mode = normalize_mode(mode)
    params = {k: v for k, v in run.items() if k not in RUN_RESERVED}
    cores = core_cost(params)
    args = [str(bin_dir / binary), *flag_args(params), "--mode", mode]
    return Job(
        case_id=case_id(protocol, params, mode),
        protocol=protocol,
        binary=binary,
        mode=mode,
        args=tuple(args),
        cores=cores,
        params=params,
    )


def jobs_from_config(config, selected_protocols, selected_modes, match_patterns, bin_dir):
    protocols = protocol_table(config)
    jobs = []
    for run in config.get("run", []):
        protocol = run.get("protocol")
        if selected_protocols and protocol not in selected_protocols:
            continue
        for mode in run.get("modes", ["full"]):
            if selected_modes and mode not in selected_modes:
                continue
            job = build_job(protocols, bin_dir, run, mode)
            if match_patterns and not any(fnmatch.fnmatch(job.case_id, p) for p in match_patterns):
                continue
            jobs.append(job)
    return jobs


def parse_run_one(protocols, bin_dir, argv):
    if not argv:
        raise SystemExit("run-one requires a protocol name")
    protocol = argv[0]
    if protocol not in protocols:
        raise SystemExit(f"unknown protocol {protocol!r}")
    rest = argv[1:]
    if rest and rest[0] == "--":
        rest = rest[1:]
    mode = "full"
    params = {}
    i = 0
    while i < len(rest):
        token = rest[i]
        if not token.startswith("--"):
            raise SystemExit(f"unexpected positional argument {token!r}")
        key = token[2:].replace("-", "_")
        if key == "mode":
            if i + 1 >= len(rest):
                raise SystemExit("--mode requires a value")
            mode = normalize_mode(rest[i + 1])
            i += 2
        elif i + 1 < len(rest) and not rest[i + 1].startswith("--"):
            params[key] = coerce_scalar(rest[i + 1])
            i += 2
        else:
            params[key] = True
            i += 1

    binary = protocols[protocol].get("binary")
    if not binary:
        raise SystemExit(f"protocol {protocol!r} must define binary")
    args = [str(bin_dir / binary), *rest]
    if "--mode" not in rest:
        args.extend(["--mode", mode])
    return [
        Job(
            case_id=case_id(protocol, params, mode),
            protocol=protocol,
            binary=binary,
            mode=mode,
            args=tuple(args),
            cores=core_cost(params),
            params=params,
        )
    ]


def coerce_scalar(value):
    if value in {"true", "false"}:
        return value == "true"
    try:
        return int(value)
    except ValueError:
        return value


def validate_jobs(jobs, available_cores):
    seen = {}
    for job in jobs:
        if job.case_id in seen:
            raise ValueError(f"duplicate case id {job.case_id!r}")
        seen[job.case_id] = job
        if job.cores > available_cores:
            raise ValueError(
                f"{job.case_id} requests {job.cores} cores, "
                f"but available_cores is {available_cores}"
            )


def wrap_time(args):
    time_bin = shutil.which("time")
    if not time_bin:
        return list(args), None
    if sys.platform == "darwin":
        return [time_bin, "-l", *args], "darwin"
    return [time_bin, "-v", *args], "linux"


def parse_max_rss_kb(mode, stderr):
    if mode == "linux":
        match = re.search(r"Maximum resident set size \(kbytes\):\s*(\d+)", stderr)
        return int(match.group(1)) if match else None
    if mode == "darwin":
        match = re.search(r"^\s*(\d+)\s+maximum resident set size", stderr, re.MULTILINE)
        if match:
            return (int(match.group(1)) + 1023) // 1024
    return None


def parse_stats(stdout):
    matches = re.findall(r"Stats\s*=\s*(\d+)\s*,\s*(\d+)", stdout)
    if not matches:
        return -1, -1
    execs, blocked = matches[-1]
    return int(execs), int(blocked)


class OutputBuffer:
    def __init__(self):
        self._chunks = []
        self._lock = threading.Lock()

    def append(self, chunk):
        with self._lock:
            self._chunks.append(chunk)

    def text(self):
        with self._lock:
            return b"".join(self._chunks).decode("utf-8", errors="replace")


def read_stream(stream, output):
    try:
        while True:
            chunk = os.read(stream.fileno(), 65536)
            if not chunk:
                break
            output.append(chunk)
    finally:
        stream.close()


def write_stdout_snapshots(case_dir, output, stop_event, interval_seconds):
    if interval_seconds <= 0:
        return

    snapshots_dir = case_dir / "snapshots"
    index = 1
    while not stop_event.wait(interval_seconds):
        snapshots_dir.mkdir(parents=True, exist_ok=True)
        snapshot = snapshots_dir / f"stdout-{index}.txt"
        snapshot.write_text(output.text(), encoding="utf-8", errors="replace")
        index += 1


def register_proc(job, proc):
    with ACTIVE_PROCS_LOCK:
        ACTIVE_PROCS[proc.pid] = (job, proc)


def unregister_proc(proc):
    with ACTIVE_PROCS_LOCK:
        ACTIVE_PROCS.pop(proc.pid, None)


def kill_process_group(proc, sig):
    if proc.poll() is not None:
        return
    try:
        if hasattr(os, "killpg"):
            os.killpg(proc.pid, sig)
        else:
            proc.send_signal(sig)
    except ProcessLookupError:
        pass


def terminate_active_processes(sig=signal.SIGTERM):
    with ACTIVE_PROCS_LOCK:
        procs = list(ACTIVE_PROCS.values())
    for _, proc in procs:
        kill_process_group(proc, sig)


def handle_termination(signum, _frame):
    global TERMINATING, TERMINATION_SIGNAL
    if TERMINATING:
        terminate_active_processes(signal.SIGKILL)
        raise SystemExit(128 + signum)
    TERMINATING = True
    TERMINATION_SIGNAL = signum
    with ACTIVE_PROCS_LOCK:
        active = list(ACTIVE_PROCS.values())
    print(
        f"[signal] received {signum}; terminating {len(active)} active job(s)",
        file=sys.stderr,
        flush=True,
    )
    for _, proc in active:
        kill_process_group(proc, signal.SIGTERM)


def install_signal_handlers():
    signal.signal(signal.SIGTERM, handle_termination)
    signal.signal(signal.SIGINT, handle_termination)


def print_job_output(job, stdout, stderr, label, max_lines=200):
    def tail(text):
        lines = text.splitlines()
        if len(lines) <= max_lines:
            return "\n".join(lines)
        return "\n".join([f"... truncated to last {max_lines} lines ...", *lines[-max_lines:]])

    print(f"\n[{label}] {job.case_id} stdout:", flush=True)
    print(tail(stdout), flush=True)
    if stderr.strip():
        print(f"\n[{label}] {job.case_id} stderr:", flush=True)
        print(tail(stderr), flush=True)


def write_time_file(path, job, status, exit_code, elapsed, timeout_hours, max_rss_kb, execs, blocked):
    lines = [
        f"case = {toml_quote(job.case_id)}",
        f"protocol = {toml_quote(job.protocol)}",
        f"binary = {toml_quote(job.binary)}",
        f"mode = {toml_quote(job.mode)}",
        f"status = {toml_quote(status)}",
        f"exit_code = {exit_code}",
        f"elapsed_seconds = {elapsed:.6f}",
        f"timeout_hours = {timeout_hours}",
        f"cores = {job.cores}",
        f"execs = {execs}",
        f"blocked = {blocked}",
    ]
    if max_rss_kb is not None:
        lines.append(f"max_rss_kb = {max_rss_kb}")
    lines.append("command = [")
    for arg in job.args:
        lines.append(f"  {toml_quote(arg)},")
    lines.append("]")
    path.write_text("\n".join(lines) + "\n")


def run_job(job, results_dir, timeout_seconds, timeout_hours, snapshot_interval_seconds):
    case_dir = results_dir / job.case_id
    if case_dir.exists():
        shutil.rmtree(case_dir)
    case_dir.mkdir(parents=True)
    (case_dir / "command.txt").write_text(" ".join(shlex_quote(a) for a in job.args) + "\n")

    cmd, time_mode = wrap_time(job.args)
    start = time.perf_counter()
    timed_out = False
    stdout_buffer = OutputBuffer()
    stderr_buffer = OutputBuffer()
    stop_snapshots = threading.Event()
    proc = subprocess.Popen(
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        preexec_fn=os.setsid if hasattr(os, "setsid") else None,
    )
    stdout_reader = threading.Thread(
        target=read_stream,
        args=(proc.stdout, stdout_buffer),
        daemon=True,
    )
    stderr_reader = threading.Thread(
        target=read_stream,
        args=(proc.stderr, stderr_buffer),
        daemon=True,
    )
    snapshot_writer = threading.Thread(
        target=write_stdout_snapshots,
        args=(case_dir, stdout_buffer, stop_snapshots, snapshot_interval_seconds),
        daemon=True,
    )
    stdout_reader.start()
    stderr_reader.start()
    snapshot_writer.start()

    register_proc(job, proc)
    try:
        proc.wait(timeout=timeout_seconds)
    except subprocess.TimeoutExpired:
        timed_out = True
        kill_process_group(proc, signal.SIGKILL)
        proc.wait()
    finally:
        unregister_proc(proc)
        stop_snapshots.set()
        stdout_reader.join()
        stderr_reader.join()
        snapshot_writer.join()

    elapsed = time.perf_counter() - start
    exit_code = proc.returncode if proc.returncode is not None else -1
    status = "timeout" if timed_out else ("ok" if exit_code == 0 else f"exit_code_{exit_code}")
    stdout = stdout_buffer.text()
    stderr = stderr_buffer.text()
    max_rss_kb = parse_max_rss_kb(time_mode, stderr)
    execs, blocked = parse_stats(stdout)

    (case_dir / "stdout.txt").write_text(stdout, encoding="utf-8", errors="replace")
    (case_dir / "stderr.txt").write_text(stderr, encoding="utf-8", errors="replace")
    write_time_file(
        case_dir / "time.toml",
        job,
        status,
        exit_code,
        elapsed,
        timeout_hours,
        max_rss_kb,
        execs,
        blocked,
    )

    if timed_out:
        print_job_output(job, stdout, stderr, "timeout-output")
    elif TERMINATING and exit_code != 0:
        print_job_output(job, stdout, stderr, "interrupted-output")

    return {
        "job": job,
        "status": status,
        "elapsed": elapsed,
        "execs": execs,
        "blocked": blocked,
    }


def shlex_quote(value):
    import shlex

    return shlex.quote(value)


def run_scheduled(jobs, cfg):
    validate_jobs(jobs, cfg["available_cores"])
    jobs = sorted(jobs, key=lambda j: (j.cores, j.case_id))
    pending = list(jobs)
    running = {}
    used_cores = 0
    max_workers = min(cfg["jobs"], len(jobs)) or 1
    cfg["results_dir"].mkdir(parents=True, exist_ok=True)

    print(f"selected_jobs = {len(jobs)}")
    print(f"available_cores = {cfg['available_cores']}")
    print(f"max_processes = {max_workers}")

    with ThreadPoolExecutor(max_workers=max_workers) as executor:
        while pending or running:
            if TERMINATING:
                pending.clear()

            launched = False
            i = 0
            while not TERMINATING and i < len(pending) and len(running) < max_workers:
                job = pending[i]
                if used_cores + job.cores <= cfg["available_cores"]:
                    pending.pop(i)
                    used_cores += job.cores
                    print(f"[start] {job.case_id} cores={job.cores}", flush=True)
                    fut = executor.submit(
                        run_job,
                        job,
                        cfg["results_dir"],
                        cfg["timeout_seconds"],
                        cfg["timeout_hours"],
                        cfg["snapshot_interval_seconds"],
                    )
                    running[fut] = job
                    launched = True
                else:
                    i += 1

            if not running:
                if TERMINATING:
                    break
                raise RuntimeError("no runnable jobs despite non-empty pending queue")

            if launched and pending and not TERMINATING:
                continue

            done, _ = wait(running.keys(), return_when=FIRST_COMPLETED)
            for fut in done:
                job = running.pop(fut)
                used_cores -= job.cores
                result = fut.result()
                print(
                    f"[done] {job.case_id} {result['elapsed']:.2f}s "
                    f"{result['status']} Stats={result['execs']},{result['blocked']}",
                    flush=True,
                )

    if TERMINATING:
        raise SystemExit(128 + (TERMINATION_SIGNAL or signal.SIGTERM))


def print_jobs(jobs):
    for job in jobs:
        print(f"{job.case_id} cores={job.cores}")
        print("  " + " ".join(shlex_quote(a) for a in job.args))
    print(f"total = {len(jobs)}")


def parse_common(parser):
    parser.add_argument("--config", default=CONFIG)
    parser.add_argument("--bin-dir")
    parser.add_argument("--results-dir")
    parser.add_argument("--available-cores", type=int)
    parser.add_argument("--jobs", type=int)
    parser.add_argument("--timeout-hours", type=float)
    parser.add_argument("--snapshot-interval-hours", type=float)


def main():
    install_signal_handlers()

    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="cmd", required=True)

    list_p = sub.add_parser("list")
    parse_common(list_p)
    list_p.add_argument("--protocol", action="append", default=[])
    list_p.add_argument("--mode", action="append", default=[])
    list_p.add_argument("--match", action="append", default=[])

    run_p = sub.add_parser("run")
    parse_common(run_p)
    run_p.add_argument("--all", action="store_true")
    run_p.add_argument("--protocol", action="append", default=[])
    run_p.add_argument("--mode", action="append", default=[])
    run_p.add_argument("--match", action="append", default=[])

    run_one_p = sub.add_parser("run-one")
    parse_common(run_one_p)
    run_one_p.add_argument("protocol")
    run_one_p.add_argument("protocol_args", nargs=argparse.REMAINDER)

    args = parser.parse_args()
    config = load_config(args.config)
    cfg = settings(config, args)
    protocols = protocol_table(config)

    if args.cmd == "list":
        jobs = jobs_from_config(config, set(args.protocol), set(args.mode), args.match, cfg["bin_dir"])
        validate_jobs(jobs, max(cfg["available_cores"], max((j.cores for j in jobs), default=1)))
        print_jobs(jobs)
        return

    if args.cmd == "run":
        if not args.all and not args.protocol and not args.mode and not args.match:
            raise SystemExit("use --all, --protocol, --mode, or --match")
        jobs = jobs_from_config(config, set(args.protocol), set(args.mode), args.match, cfg["bin_dir"])
        run_scheduled(jobs, cfg)
        return

    if args.cmd == "run-one":
        jobs = parse_run_one(protocols, cfg["bin_dir"], [args.protocol, *args.protocol_args])
        run_scheduled(jobs, cfg)
        return


if __name__ == "__main__":
    main()
