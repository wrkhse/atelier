use atelier_reporter::GateCheck;
use atelier_reporter::report::{Action, CiReport, Node, Params, results};
use gungraun::prelude::*;
use std::hint::black_box;

fn setup_report(count: u64) -> (CiReport, [String; 1]) {
    let mut actions = Vec::new();

    for _ in 0..black_box(count).clamp(1, 3) {
        actions.push(Action {
            node: Node {
                action: "run-task".to_owned(),
                params: Some(Params {
                    target: Some("reporter:fmt".to_owned()),
                }),
            },
            status: "passed".to_owned(),
            label: Some("RunTask(reporter:fmt)".to_owned()),
        });
    }

    (CiReport { actions }, ["rust/fmt".to_owned()])
}

#[library_benchmark]
#[bench::case(args = (3_u64), setup = setup_report)]
fn bench_results(input: (CiReport, [String; 1])) -> Vec<GateCheck> {
    let (report, gates) = &input;

    black_box(results(black_box(report), black_box(gates)))
}

library_benchmark_group!(name = reporter_benches, benchmarks = bench_results);

main!(library_benchmark_groups = reporter_benches);
