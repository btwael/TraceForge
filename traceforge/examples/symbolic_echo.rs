use traceforge::{recv_msg_block, send_msg, symbolic, thread, verify, Config};

fn program() {
    // Thread ids in this execution:
    let main_id = thread::current_id();
    let t2 = thread::spawn(move || {
        let v: symbolic::SymExpr = recv_msg_block();
        send_msg(main_id, v);
    });

    let t3 = thread::spawn(move || {
        let v: symbolic::SymExpr = recv_msg_block();
        send_msg(main_id, v);
    });

    let worker2_id = t2.thread().id();
    let worker3_id = t3.thread().id();

    // Broadcaster logic in main thread
    let b: symbolic::SymExpr = symbolic::fresh_bool();
    send_msg(worker2_id, b);
    send_msg(worker3_id, b);

    let x1: symbolic::SymExpr = recv_msg_block();
    let x2: symbolic::SymExpr = recv_msg_block();

    assert_eq!(x1, x2);
    symbolic::assert(x1.eq(x2));

    t2.join().unwrap();
    t3.join().unwrap();
}

fn main() {
    let stats = verify(
        Config::builder()
            .with_graph_printing("./tex")
            .with_condpor(true)
            .build(),
        program,
    );
    println!("Stats = {}, {}", stats.execs, stats.block);
}
