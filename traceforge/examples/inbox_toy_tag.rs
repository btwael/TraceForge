use traceforge::thread;

fn tagged_inbox_example(senders: u32, accepted_tag: u32) -> traceforge::Stats {
    traceforge::verify(traceforge::Config::builder().with_graph_printing("./tex").build(), move || {
        let inbox_thread = thread::spawn(move || {
            let messages = traceforge::inbox_with_tag(move |_tid, tag| tag == Some(accepted_tag));

            match messages.len() {
                0 => {},
                1 => {
                    let Some(val) = &messages[0] else {
                        panic!("expected a value, found None");
                    };
                    let value = val
                        .as_any_ref()
                        .downcast_ref::<u32>()
                        .expect("inbox should yield the tagged u32");
                    if *value != accepted_tag {
                        panic!("expected value {accepted_tag} but inbox returned {value}");
                    }
                }
                _ => {
                    panic!(
                        "expected exactly one message, got {}: {:?}",
                        messages.len(),
                        messages
                    );
                }
            };
        });

        let inbox_tid = inbox_thread.thread().id();
        let mut sender_threads = Vec::new();

        for tag in 0..senders {
            let inbox_tid = inbox_tid.clone();
            sender_threads.push(thread::spawn(move || {
                traceforge::send_tagged_msg(inbox_tid, tag, tag);
            }));
        }

        for sender_thread in sender_threads {
            let _ = sender_thread.join();
        }

        let _ = inbox_thread.join();
    })
}

fn example() -> traceforge::Stats {
    tagged_inbox_example(3, 2)
}

fn forge() {
    println!("Running the example in systematic mode");
    let stats = example();
    println!("Stats = {}, {}", stats.execs, stats.block);
}

fn main() {
    forge();
}
