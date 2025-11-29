//extern crate traceforge;

use traceforge::thread;

#[derive(Clone, Debug, PartialEq)]
enum Msg {
    Work,
    Terminate,
}

fn example() {
    let t1 = thread::spawn_daemon(move || {
        let mut i = 0;
        loop {
            let m = traceforge::recv_msg_block();
            match m {
                Msg::Work => i = i + 1,
                Msg::Terminate => assert!(i < 10),
            }
        }
    });
    let t1_id = t1.thread().id();

    let t2 = thread::spawn(move || {
        traceforge::send_msg(t1_id, Msg::Work);
        traceforge::send_msg(t1_id, Msg::Work);
        traceforge::send_msg(t1_id, Msg::Work);
        traceforge::send_msg(t1_id, Msg::Work);
        traceforge::send_msg(t1_id, Msg::Work);
        traceforge::send_msg(t1_id, Msg::Work);
        traceforge::send_msg(t1_id, Msg::Work);
        traceforge::send_msg(t1_id, Msg::Work);
        traceforge::send_msg(t1_id, Msg::Work);
        traceforge::send_msg(t1_id, Msg::Work);
    });
    traceforge::send_msg(t1_id.clone(), Msg::Terminate);

    let _ = t1.join();
    let _ = t2.join();
}

fn random() {
    let ntests = 10;
    println!("Running the example in random mode {ntests} times");
    let _ = traceforge::test(traceforge::Config::builder().build(), example, ntests);
}

fn forge() {
    println!("Running the example in systematic mode");
    let stats = traceforge::verify(traceforge::Config::builder().build(), example);
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
