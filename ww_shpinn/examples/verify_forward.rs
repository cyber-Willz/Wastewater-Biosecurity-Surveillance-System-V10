//! Compare the linear-feature PINN with the analytic Gaussian ADR solution.
//! `cargo run --release -p ww_shpinn --example verify_forward`
use ww_shpinn::{solve_forward, Adr, PielmConfig};

fn main() {
    for (name, k) in [("SARS-CoV-2 (k=0.50)", 0.50), ("Amoxicillin (k=0.15)", 0.15), ("PFOA (k=0.005)", 0.005)] {
        let adr = Adr { v: 1.0, d: 0.05, k, length: 6.0, horizon: 1.5 };
        let reference = |x: f64, t: f64| adr.gaussian_reference(1.5, 0.5, x, t);
        let cfg = PielmConfig::for_adr(&adr, 0.5);
        let sol = solve_forward(&adr, &|_, _| 0.0, &|x| reference(x, 0.0),
            &|t| reference(0.0, t), &|t| reference(6.0, t), &[], &cfg).expect("solve");
        let err = sol.max_abs_error(&reference, 60, 30);
        let res = sol.sup_residual_l2(&|_, _| 0.0, 60, 30);
        println!("{name:22} max|c_θ − c| = {err:.4}   sup‖R‖₂ = {res:.4}   κ = {:.3}", adr.kappa());
    }
}
