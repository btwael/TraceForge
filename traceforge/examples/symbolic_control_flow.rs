use traceforge::{recv_msg_block, send_msg, symbolic, thread, verify, Config};

fn program() {
    // Thread ids in this execution:
    let main_id = thread::current_id();

    let t2 = thread::spawn(move || {
        let v: symbolic::SymExpr = recv_msg_block();

        let ok = symbolic::eval((v % 2).eq(symbolic::int_val(0)));

        send_msg(main_id, ok);
    });

    let worker2_id = t2.thread().id();

    let i: symbolic::SymExpr = symbolic::fresh_int();
    send_msg(worker2_id, i);

    let ok: bool = recv_msg_block();

    if ok {
        symbolic::assert((i % 2).eq(symbolic::int_val(0)));
    }

    t2.join().unwrap();
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
