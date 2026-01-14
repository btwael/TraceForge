//extern crate traceforge;
use traceforge::thread;
use traceforge::Nondet;
use std::time::Instant;

#[derive(Clone, Debug, PartialEq)]
struct Msg {}

fn simple_inbox(inboxes: usize, senders: usize) -> traceforge::Stats {
    traceforge::verify(
        traceforge::Config::builder()
            .with_graph_printing("./tex")
            .build(),
        move || {
            let inbx_thread = thread::spawn(move || {
                for i in 0..inboxes {
                    let _ = traceforge::inbox_with_bounds(2, None);
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
        },
    )
}

fn simple_inbox_w_rcv(inboxes: usize, senders: usize) -> traceforge::Stats {
    traceforge::verify(
        traceforge::Config::builder()
            .with_graph_printing("./tex")
            .build(),
        move || {
            let inbx_thread = thread::spawn(move || {
                for i in 0..inboxes {
                    let r = (0..senders).nondet();
                    for _ in 0..(r + 1) {
                        let _: Msg = traceforge::recv_msg_block();
                    }
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
        },
    )
}

fn example() -> traceforge::Stats {
    simple_inbox(2, 4)
}

fn example_w_rcv() -> traceforge::Stats {
    simple_inbox_w_rcv(2, 4)
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
