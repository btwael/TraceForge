//extern crate traceforge;
use traceforge::thread;

#[derive(Clone, Debug, PartialEq)]
struct Msg {}

fn simple_inbox(inboxes: u32, senders: u32) -> traceforge::Stats {
    traceforge::verify(traceforge::Config::builder().build(), move || {
        let inbx_thread = thread::spawn(move || {
            for i in 0..inboxes {
                let _ = traceforge::inbox();
            }
        });

        let inb_tid = inbx_thread.thread().id();

        let mut sender_threads = Vec::new();

        for i in 0..senders {
            let s = thread::spawn(move || {
                traceforge::send_msg(inb_tid, Msg {});
            });
            sender_threads.push(s);
        }

        for sender_thread in sender_threads {
            let _ = sender_thread.join();
        }

        let _ = inbx_thread.join();
    })
}

fn example() -> traceforge::Stats {
    simple_inbox(4,4)
}

fn forge() {
    println!("Running the example in systematic mode");
    let stats = example();
    println!("Stats = {}, {}", stats.execs, stats.block);
}

fn main() {
    // Get command line arguments
    let args: Vec<String> = std::env::args().collect();

    forge();
}
