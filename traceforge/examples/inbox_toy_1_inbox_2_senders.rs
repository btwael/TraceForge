//extern crate traceforge;

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

// Expected executions: 4
fn example() {
    let inbx_thread = thread::spawn(move || {
        let m = traceforge::inbox();
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

    let _ = s1.join();
    let _ = s2.join();
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
