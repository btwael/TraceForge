//extern crate traceforge;

use simplelog::{ColorChoice, ConfigBuilder, LevelFilter, TermLogger, TerminalMode};
use traceforge::thread;
use traceforge::thread::ThreadId;

#[derive(Clone, Debug, PartialEq)]
enum Action {
    Work,
    Terminate,
}

#[derive(Clone, Debug, PartialEq)]
struct Msg {
    action: Action,
    sender: i32,
}

fn example() {
    fn senders(vals: &[Option<traceforge::Val>]) -> Vec<i32> {
        vals.iter()
            .filter_map(|v| {
                v.as_ref().map(|val| {
                    val.as_any_ref()
                        .downcast_ref::<Msg>()
                        .expect("inbox message should be Msg")
                        .sender
                })
            })
            .collect()
    }

    let inbx_thread = thread::spawn(move || {
        let first = traceforge::inbox();
        let second = traceforge::inbox();
        let third = traceforge::inbox();
        println!(
            "[inbox summary] first={:?} second={:?} third={:?}",
            senders(&first),
            senders(&second),
            senders(&third)
        );
    });

    let inb_tid = inbx_thread.thread().id();

    let s1 = thread::spawn(move || {
        traceforge::send_msg(
            inb_tid,
            Msg {
                action: Action::Work,
                sender: 0,
            },
        );
    });
    let s2 = thread::spawn(move || {
        traceforge::send_msg(
            inb_tid,
            Msg {
                action: Action::Work,
                sender: 1,
            },
        );
    });
    let s3 = thread::spawn(move || {
        traceforge::send_msg(
            inb_tid,
            Msg {
                action: Action::Work,
                sender: 2,
            },
        );
    });

    let _ = s1.join();
    let _ = s2.join();
    let _ = s3.join();
    let _ = inbx_thread.join();
}

fn random() {
    println!("Running the example in random mode");
    let num = traceforge::test(traceforge::Config::builder().build(), example, 1);
    println!("Ran {num} tests");
}

fn forge() {
    println!("Running the example in systematic mode");
    let stats = traceforge::verify(
        traceforge::Config::builder()
            .with_graph_printing("./tex")
            .build(),
        example,
    );
    println!("Stats = {}, {}", stats.execs, stats.block);
}

fn main() {
    // Enable info-level logs so the structured algorithm tracing is visible.
    let _ = TermLogger::init(
        LevelFilter::Info,
        ConfigBuilder::new()
            .set_time_level(LevelFilter::Off)
            .build(),
        TerminalMode::Mixed,
        ColorChoice::Auto,
    );

    // Get command line arguments
    let args: Vec<String> = std::env::args().collect();

    // Check if any argument was provided
    if args.len() < 2 {
        println!("Usage: {} --random|--forge", args[0]);
        return;
    }

    // Match the first argument
    match args[1].as_str() {
        "--random" => random(),
        "--forge" => forge(),
        _ => println!("Invalid argument. Use --random or --forge"),
    }
}
