//! Integration tests for the kernel fusion pass (Phase 2.7).
//!
//! Tests the three edge cases from the design document:
//! 1. Split chain — soft node in the middle creates two separate fused blocks.
//! 2. Fan-out — a producer with two consumers is never fused.
//! 3. Post-fused reference — a non-fused node referencing an intermediate that
//!    was bound inside a fused block still compiles and produces correct output.

use vertexrs::{Frame, Node, pipeline};

fn make_frame(n: usize) -> Frame {
    let prices: Vec<f64> = (1..=n).map(|i| i as f64).collect();
    Frame::new().append(Node::from_data("price", prices))
}

// ── Edge case 1: split chain around a soft node ───────────────────────────────

#[test]
fn test_split_chain_around_soft_node() {
    // Chain: [a(pure), b(pure)] → soft node c → [d(pure), e(pure)]
    // Expected: two fused blocks (a,b) and (d,e), with single-node c between.
    const N: usize = 10;
    let frame = make_frame(N);

    let mut p = pipeline! {
        source!(price: f64);
        node!(a = price.row(|x| x * 2.0_f64));
        node!(b = a.row(|x| x + 1.0_f64));
        node!(c = b.row(|x| x - 0.5_f64)?);  // soft failure
        node!(d = c.row(|x| x * x));
        node!(e = d.row(|x| x / 2.0_f64));
        output!(a, b, c, d, e)
    };
    p.push(&frame);
    p.compute().unwrap();

    let out = p.output();
    let a_out = out.get::<f64>("a").expect("a");
    let b_out = out.get::<f64>("b").expect("b");
    let c_out = out.get::<f64>("c").expect("c");
    let d_out = out.get::<f64>("d").expect("d");
    let e_out = out.get::<f64>("e").expect("e");

    for i in 0..N {
        let price = (i + 1) as f64;
        let expected_a = price * 2.0;
        let expected_b = expected_a + 1.0;
        let expected_c = expected_b - 0.5;
        let expected_d = expected_c * expected_c;
        let expected_e = expected_d / 2.0;

        assert!((a_out[i] - expected_a).abs() < 1e-12, "a[{i}]");
        assert!((b_out[i] - expected_b).abs() < 1e-12, "b[{i}]");
        assert!((c_out[i] - expected_c).abs() < 1e-12, "c[{i}]");
        assert!((d_out[i] - expected_d).abs() < 1e-12, "d[{i}]");
        assert!((e_out[i] - expected_e).abs() < 1e-12, "e[{i}]");
    }
}

// ── Edge case 2: fan-out leaves producer and both consumers unfused ────────────

#[test]
fn test_fan_out_unfused() {
    // `a` is consumed by both `b` and `c`, so fanout["a"] = 2.
    // Neither the a→b nor the a→c link is fusable; all three nodes emit Single.
    const N: usize = 8;
    let frame = make_frame(N);

    let mut p = pipeline! {
        source!(price: f64);
        node!(a = price.row(|x| x * 2.0_f64));
        node!(b = a.row(|x| x + 10.0_f64));  // consumer 1 of a
        node!(c = a.row(|x| x - 1.0_f64));   // consumer 2 of a
        output!(a, b, c)
    };
    p.push(&frame);
    p.compute().unwrap();

    let out = p.output();
    let a_out = out.get::<f64>("a").expect("a");
    let b_out = out.get::<f64>("b").expect("b");
    let c_out = out.get::<f64>("c").expect("c");

    for i in 0..N {
        let price = (i + 1) as f64;
        let expected_a = price * 2.0;
        assert!((a_out[i] - expected_a).abs() < 1e-12, "a[{i}]");
        assert!((b_out[i] - (expected_a + 10.0)).abs() < 1e-12, "b[{i}]");
        assert!((c_out[i] - (expected_a - 1.0)).abs() < 1e-12, "c[{i}]");
    }
}

// ── Edge case 3: post-fused reference ─────────────────────────────────────────

#[test]
fn test_post_fused_reference() {
    // Chain [c, d] is fused (fanout["b"] = 1, fanout["c"] = 1).
    // Node `e` (pure=false) references `b` as a body dep — `b` is an earlier
    // node in the overall sequence bound by Single(b). This verifies that
    // intermediates remain accessible to non-fused nodes that follow.
    const N: usize = 6;
    let frame = make_frame(N);

    let mut p = pipeline! {
        source!(price: f64);
        node!(b = price.row(|x| x * 3.0_f64));          // Single(b): fanout["b"] checked below
        node!(c = b.row(|x| x + 1.0_f64));              // starts potential chain
        node!(d = c.row(|x| x * x));                    // extends chain → Fused([c,d])
        node!(e = d.row(|x| x + b), pure = false);      // non-fused, body refs b
        output!(b, c, d, e)
    };
    p.push(&frame);
    p.compute().unwrap();

    let out = p.output();
    let b_out = out.get::<f64>("b").expect("b");
    let c_out = out.get::<f64>("c").expect("c");
    let d_out = out.get::<f64>("d").expect("d");
    let e_out = out.get::<f64>("e").expect("e");

    for i in 0..N {
        let price = (i + 1) as f64;
        let expected_b = price * 3.0;
        let expected_c = expected_b + 1.0;
        let expected_d = expected_c * expected_c;
        let expected_e = expected_d + expected_b;

        assert!((b_out[i] - expected_b).abs() < 1e-12, "b[{i}]");
        assert!((c_out[i] - expected_c).abs() < 1e-12, "c[{i}]");
        assert!((d_out[i] - expected_d).abs() < 1e-12, "d[{i}]");
        assert!((e_out[i] - expected_e).abs() < 1e-12, "e[{i}]");
    }
}

// ── Fused 5-node pipeline correctness ─────────────────────────────────────────

#[test]
fn test_fused_5node_correctness() {
    // Verify the 5-node benchmark pipeline produces the expected values.
    const N: usize = 20;
    let frame = make_frame(N);

    let mut p = pipeline! {
        source!(price: f64);
        node!(a = price.row(|x| x * 2.0_f64));
        node!(b = a.row(|x| x + 1.0_f64));
        node!(c = b.row(|x| x - 0.5_f64));
        node!(d = c.row(|x| x * x));
        node!(e = d.row(|x| x / 2.0_f64));
        output!(a, b, c, d, e)
    };
    p.push(&frame);
    p.compute().unwrap();

    let out = p.output();
    let a_out = out.get::<f64>("a").expect("a");
    let b_out = out.get::<f64>("b").expect("b");
    let c_out = out.get::<f64>("c").expect("c");
    let d_out = out.get::<f64>("d").expect("d");
    let e_out = out.get::<f64>("e").expect("e");

    for i in 0..N {
        let price = (i + 1) as f64;
        let ea = price * 2.0;
        let eb = ea + 1.0;
        let ec = eb - 0.5;
        let ed = ec * ec;
        let ee = ed / 2.0;

        assert!(
            (a_out[i] - ea).abs() < 1e-12,
            "a[{i}]: got {} expected {ea}",
            a_out[i]
        );
        assert!(
            (b_out[i] - eb).abs() < 1e-12,
            "b[{i}]: got {} expected {eb}",
            b_out[i]
        );
        assert!(
            (c_out[i] - ec).abs() < 1e-12,
            "c[{i}]: got {} expected {ec}",
            c_out[i]
        );
        assert!(
            (d_out[i] - ed).abs() < 1e-12,
            "d[{i}]: got {} expected {ed}",
            d_out[i]
        );
        assert!(
            (e_out[i] - ee).abs() < 1e-12,
            "e[{i}]: got {} expected {ee}",
            e_out[i]
        );
    }
}
