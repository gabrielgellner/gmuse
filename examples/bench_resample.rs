// Compare rubato configurations for the fixed 48k -> 44.1k case, at realistic
// input block sizes. Reported as % of one core, which is what actually matters
// for a player running in the background.
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Async, Fft, FixedAsync, FixedSync, Indexing, PolynomialDegree, Resampler,
    SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use std::time::Instant;

const SECS: usize = 120;
const IN_RATE: usize = 48_000;
const CH: usize = 2;

fn run(name: &str, mut r: Box<dyn Resampler<f32>>, block_frames: usize) {
    let input: Vec<f32> = (0..IN_RATE * SECS * CH)
        .map(|i| (i as f32 / 97.0).sin() * 0.25)
        .collect();
    let mut staging: Vec<f32> = Vec::new();
    let mut scratch: Vec<f32> = vec![0.0; r.output_frames_max() * CH];
    let mut produced = 0usize;

    let t0 = Instant::now();
    for block in input.chunks(block_frames * CH) {
        staging.extend_from_slice(block);
        loop {
            let need = r.input_frames_next();
            if staging.len() / CH < need {
                break;
            }
            let out_max = r.output_frames_max();
            if scratch.len() < out_max * CH {
                scratch.resize(out_max * CH, 0.0);
            }
            let inp = InterleavedSlice::new(&staging[..need * CH], CH, need).unwrap();
            let mut outp = InterleavedSlice::new_mut(&mut scratch[..], CH, out_max).unwrap();
            let (nin, nout) = r
                .process_into_buffer(&inp, &mut outp, Some(&Indexing::new()))
                .unwrap();
            produced += nout;
            staging.drain(..nin * CH);
        }
    }
    let dt = t0.elapsed();
    let pct = dt.as_secs_f64() / SECS as f64 * 100.0;
    println!(
        "  {name:<38} {pct:5.2}% of a core   ({:>6.0}x rt, {produced} frames out)",
        SECS as f64 / dt.as_secs_f64()
    );
}

fn main() {
    let ratio = 44_100.0 / 48_000.0;
    for &block in &[960usize, 4096] {
        println!("\ninput blocks of {block} frames ({}ms of 48k audio):", block * 1000 / 48_000);

        let p = SincInterpolationParameters::default(); // sinc_len 256, BH2, cubic
        run("sinc 256 cubic (current)",
            Box::new(Async::<f32>::new_sinc(ratio, 1.1, &p, 1024, CH, FixedAsync::Input).unwrap()), block);

        let p128 = SincInterpolationParameters::new(128, WindowFunction::BlackmanHarris2);
        run("sinc 128 cubic",
            Box::new(Async::<f32>::new_sinc(ratio, 1.1, &p128, 1024, CH, FixedAsync::Input).unwrap()), block);

        let p64l = SincInterpolationParameters::new(64, WindowFunction::BlackmanHarris2)
            .interpolation(SincInterpolationType::Linear);
        run("sinc 64 linear",
            Box::new(Async::<f32>::new_sinc(ratio, 1.1, &p64l, 1024, CH, FixedAsync::Input).unwrap()), block);

        run("poly cubic",
            Box::new(Async::<f32>::new_poly(ratio, 1.1, PolynomialDegree::Cubic, 1024, CH, FixedAsync::Input).unwrap()), block);

        run("poly septic",
            Box::new(Async::<f32>::new_poly(ratio, 1.1, PolynomialDegree::Septic, 1024, CH, FixedAsync::Input).unwrap()), block);

        run("FFT (fixed ratio)",
            Box::new(Fft::<f32>::new(IN_RATE, 44_100, 1024, CH, FixedSync::Input).unwrap()), block);
    }
}
