use std::{env, process, time::Duration};

use examples::two_pc::{self, udp::UdpTransport, TwoPcRound};
use traceforge_rounds::Comm;

fn main() {
    if let Err(err) = run() {
        eprintln!("{err}");
        process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args = env::args().skip(1);
    let role = args.next().ok_or_else(usage)?;
    let options = Options::parse(args.collect::<Vec<_>>())?;

    match role.as_str() {
        "coordinator" => {
            let port = options.required_port("--port")?;
            let participants = options.required_ports("--participants")?;
            let rounds = options.required_u32("--rounds")?;
            let transport = UdpTransport::bind(port)
                .map_err(|err| format!("failed to bind UDP port {port}: {err}"))?
                .with_inbox_timeout(Duration::from_secs(2));
            let mut comm = Comm::<TwoPcRound, _>::new(transport);

            two_pc::coordinator_with_decisions(
                &mut comm,
                port,
                &participants,
                rounds,
                |round, commit| {
                    println!(
                        "coordinator round {round}: decision {}",
                        if commit { "commit" } else { "abort" }
                    );
                },
            )
            .map_err(|err| format!("coordinator failed: {err}"))?;
            println!("coordinator finished {rounds} rounds");
        }
        "participant" => {
            let port = options.required_port("--port")?;
            let coordinator = options.required_port("--coordinator")?;
            let votes = options.required_votes("--votes")?;
            let rounds = votes.len() as u32;
            let transport = UdpTransport::bind(port)
                .map_err(|err| format!("failed to bind UDP port {port}: {err}"))?
                .with_recv_timeout(Duration::from_secs(2));
            let mut comm = Comm::<TwoPcRound, _>::new(transport);

            two_pc::participant(&mut comm, rounds, |round| votes[round as usize])
                .map_err(|err| format!("participant failed: {err}"))?;
            println!("participant {port} finished {rounds} rounds with coordinator {coordinator}");
        }
        _ => return Err(usage()),
    }

    Ok(())
}

#[derive(Debug)]
struct Options {
    pairs: Vec<(String, String)>,
}

impl Options {
    fn parse(args: Vec<String>) -> Result<Self, String> {
        let mut pairs = Vec::new();
        let mut iter = args.into_iter();

        while let Some(flag) = iter.next() {
            if !flag.starts_with("--") {
                return Err(format!("unexpected argument `{flag}`\n\n{}", usage()));
            }
            let value = iter
                .next()
                .ok_or_else(|| format!("missing value for `{flag}`\n\n{}", usage()))?;
            pairs.push((flag, value));
        }

        Ok(Self { pairs })
    }

    fn required_port(&self, flag: &str) -> Result<u16, String> {
        let value = self.required(flag)?;
        value
            .parse::<u16>()
            .map_err(|_| format!("`{flag}` must be a UDP port, got `{value}`"))
    }

    fn required_u32(&self, flag: &str) -> Result<u32, String> {
        let value = self.required(flag)?;
        value
            .parse::<u32>()
            .map_err(|_| format!("`{flag}` must be an integer, got `{value}`"))
    }

    fn required_ports(&self, flag: &str) -> Result<Vec<u16>, String> {
        parse_csv(self.required(flag)?, flag, |item| {
            item.parse::<u16>()
                .map_err(|_| format!("`{flag}` contains invalid port `{item}`"))
        })
    }

    fn required_votes(&self, flag: &str) -> Result<Vec<bool>, String> {
        parse_csv(self.required(flag)?, flag, |item| match item {
            "yes" | "y" | "true" | "1" => Ok(true),
            "no" | "n" | "false" | "0" => Ok(false),
            _ => Err(format!(
                "`{flag}` contains invalid vote `{item}`; use yes/no"
            )),
        })
    }

    fn required(&self, flag: &str) -> Result<&str, String> {
        self.pairs
            .iter()
            .find_map(|(key, value)| (key == flag).then_some(value.as_str()))
            .ok_or_else(|| format!("missing `{flag}`\n\n{}", usage()))
    }
}

fn parse_csv<T, F>(value: &str, flag: &str, mut parse: F) -> Result<Vec<T>, String>
where
    F: FnMut(&str) -> Result<T, String>,
{
    if value.is_empty() {
        return Err(format!("`{flag}` must not be empty"));
    }

    value.split(',').map(|item| parse(item.trim())).collect()
}

fn usage() -> String {
    "\
usage:
  two_pc_udp coordinator --port 9000 --participants 9001,9002,9003 --rounds 3
  two_pc_udp participant --port 9001 --coordinator 9000 --votes yes,no,yes"
        .to_owned()
}
