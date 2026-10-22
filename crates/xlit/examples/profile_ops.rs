//! Per-op time breakdown of one decoder step (B=5, P=10):
//! `cargo run --profile fast --example profile_ops`
use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use tract_onnx::prelude::*;

fn main() -> TractResult<()> {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets");
    let enc = tract_onnx::onnx()
        .model_for_path(assets.join("xlit_encoder.onnx"))?
        .into_optimized()?
        .into_runnable()?;
    let src = Tensor::from_shape(
        &[1, 14],
        &[51i64, 17, 4, 23, 6, 11, 4, 23, 11, 12, 21, 4, 16, 2],
    )?;
    let mut cross = enc.run(tvec![src.into()])?;
    let (ck, cv) = (cross.remove(0), cross.remove(0));
    let (b, p) = (5usize, 10usize);
    let tokens = Tensor::from_shape(&[b, 1], &vec![10i64; b])?;
    let past = Tensor::zero::<f32>(&[6, b, 4, p, 64])?;
    let inputs = || {
        tvec![
            tokens.clone().into(),
            past.clone().into(),
            past.clone().into(),
            ck.clone(),
            cv.clone()
        ]
    };
    let dec = tract_onnx::onnx()
        .model_for_path(assets.join("xlit_decoder.onnx"))?
        .into_optimized()?
        .into_runnable()?;
    let mut state = dec.spawn()?;
    let mut by_op: HashMap<String, (f64, usize)> = HashMap::new();
    let rounds = 50;
    for r in 0..=rounds {
        state.run_plan_with_eval(inputs(), |ctx, st, node, input| {
            let t = Instant::now();
            let out = tract_onnx::tract_core::plan::eval(ctx, st, node, input);
            if r > 0 {
                let name = node.op().name().to_string();
                let e = by_op.entry(name).or_default();
                e.0 += t.elapsed().as_secs_f64() * 1e3 / rounds as f64;
                e.1 += 1;
            }
            out
        })?;
    }
    let mut v: Vec<_> = by_op.into_iter().collect();
    v.sort_by(|a, b| b.1.0.total_cmp(&a.1.0));
    let total: f64 = v.iter().map(|x| x.1.0).sum();
    println!("total {total:.3} ms/step");
    for (name, (ms, n)) in v.iter().take(15) {
        println!("{name:30} {ms:7.3} ms  ({} nodes)", n / rounds);
    }
    Ok(())
}
