# ww_biosec v0.3— Mathematical, Physical and Spectral-Hypergraph Foundations

*Formal account of how the wastewater biosurveillance system in `ww_biosec_v0_3` works: the physics it encodes, the statistics of its temporal detector, the spectral hypergraph theory behind its network score, and the Physics-Informed Neural Network (DSPINN) layer its schema is designed to feed.*

> **Version note.** §0–§8 and Appendices A–B analyse **v0.3 exactly as received**. The corrections made in **v0.4** (this archive) and their measured effect are in **Appendix C**; `ww_shpinn` implements a solvable subset of §7.



---

## 0. Scope, evidence, and the one thing you should know first

### 0.1 What was examined and how

| Step | What was done |
|---|---|
| Source review | Every `.rs`/`.py` file in the five workspace crates (`ww_domain`, `ww_detection`, `ww_audit`, `ww_alert_agent`, `ww_runner`) and the two vendored crates (`spectral_hypergraph`, `ontology_engine`) was read. |
| Build and run | The workspace was built with Rust 1.75 and the `ww_biosec` binary was run (no API key, so the triage step used the deterministic fallback in `agent_bridge.py`, not a live model call). |
| Independent replica | The detection mathematics (LCG noise, pulses, EWMA/Welford, Zhou Laplacian, spectral score, severity map) was re-implemented in Python (numpy/sympy). **The replica reproduces all 51 alerts of the real binary** — day, analyte, site, $z$, spectral score and severity — to displayed precision. |
| Exact algebra | The Belize hypergraph Laplacian spectrum was derived symbolically (sympy), not just numerically. |

Numbers marked ✔ below come from the binary or the replica. Nothing marked ✔ is an estimate from memory.

### 0.2 Status labels

| Label | Meaning |
|---|---|
| **[IMPL]** | Mathematics that the shipped code actually executes. Theorems here are statements about the code. |
| **[PHYS]** | Physical model named in code comments (advection–diffusion–decay) but **not integrated** by any code. |
| **[SPEC]** | Design specification (SHPINN) derived from the interface the code exposes. **No PINN code is in the archive.** |

### 0.3 The PINN question, answered directly

A search of the whole archive finds **no neural network, no autodiff, no loss function, no collocation points**. "SHPINN" appears only in comments and schema docs, and its sole functional footprint is one number per signal: the ontology property `decay_rate_k` ("k parameter for SHPINN PDE regression"). So the honest structure of this document is:

* §2–§6 prove how the system **as shipped** works (statistics + spectral hypergraph + physics-derived α).
* §7 gives the complete SHPINN formulation and stability proofs that the schema is set up to support, clearly labelled **[SPEC]**.

I have not written up the PINN as if it were part of the running system, because it is not.

### 0.4 Headline results (all ✔)

1. **The spectral score is not baseline-invariant.** It is computed on raw log₁₀ concentrations, but the Laplacian's null vector is $D_v^{1/2}\mathbf 1$, not $\mathbf 1$. Consequently $S(u+a\mathbf 1)\neq S(u)$, and whether the "network-spread" upgrade can ever fire depends on $|\mu_a|/\sigma_a$ (units and level), not on epidemiology (§4.7–4.9).
2. **The documented range is wrong.** $R(u)/2\in[0,\tfrac12]$, not $[0,1]$, because $\operatorname{spec}(\Delta)\subset[0,1]$ for the Zhou Laplacian. The composite score satisfies $S\in[0,0.7]$ exactly (§4.5).
3. **Exact spectrum:** $\operatorname{spec}(\Delta)=\{0,\tfrac1{21},\tfrac9{14},1^{(5)}\}$, Fiedler value $\lambda_2=1/21\approx0.0476$ (matches the binary's printed `λ₂=0.0476`).
4. **Detection outcome in the shipped run:** 51 alerts, 26 of them at (site, analyte) pairs with **no injected pulse**; the flagship SARS-CoV-2 source site (`bcity_south`, amplitude 2.8) is **never alerted**; Ciprofloxacin at San Ignacio is also missed (§3.6, §6).
5. **Physics link that is exactly true:** with $\alpha=1-e^{-k\Delta t}$ the EWMA is the exact zero-order-hold discretisation of a first-order relaxation ODE with the analyte's own rate constant (§3.3).

---

## 1. The system as a mathematical object

### 1.1 Data flow

```
 generative model (scenario.rs)            per (day t, analyte a)
 y[s,a,t] = max(μ_a + σ_a ξ + pulses, −2)  ───►  field u_{a,t} ∈ ℝ^8  (one entry per site)
                                                   │
                       ┌───────────────────────────┴───────────────────────┐
                       ▼                                                   ▼
        per-site temporal detector                           network spectral score
        EWMA + Welford → z[s,a,t]                            S(u) = 0.6·RQ + 0.4·HFF
                       └───────────────────────────┬───────────────────────┘
                                                   ▼
                          severity map  σ(z, S; z_thr)  →  alert (ontology object)
                                                   ▼
                    triage (rules / LLM bridge) → review → audit log (NDJSON)
```

### 1.2 Notation

| Symbol | Meaning | Code |
|---|---|---|
| $s\in\{1..n\}$, $n=8$ | monitoring site | `SITES` |
| $a\in\{1..18\}$ | analyte (4 categories) | `ANALYTE_CATALOG` |
| $t\in\{0..14\}$ | day | `N_DAYS=15` |
| $y_{s,a,t}$ | $\log_{10}$ concentration | `log10_copies_per_l` |
| $\mu_a,\sigma_a$ | endemic baseline, noise s.d. (log₁₀) | `baseline_log`, `noise_std` |
| $k_a$ | first-order decay rate (d⁻¹) | `decay_rate_k` |
| $\alpha_a$ | EWMA factor | `ewma_alpha()` |
| $z_{s,a,t}$ | z-score | `BaselineUpdate.z_score` |
| $u_{a,t}\in\mathbb R^n$ | site-indexed field for one analyte-day | `concs` |
| $H,W,D_e,D_v$ | incidence, edge weights, edge cardinalities, weighted vertex degrees | `spectral_hypergraph` |
| $\Delta$ | normalised hypergraph Laplacian | `dense_normalized_laplacian` |
| $S$ | composite spectral score | `spectral_score` |

### 1.3 Generative model of the simulation [IMPL]

For site $s$, analyte $a$, day $t$:

$$
y_{s,a,t}=\max\!\Big(\mu_a+\sigma_a\,\xi_{s,a,t}+\sum_{p\in\mathcal P_{s,a}}A_p\,e^{-\frac{(t-t_p)^2}{2\varsigma_p^2}},\;-2\Big),
\qquad
\xi=\sum_{j=1}^{12}U_j-6 .
$$

$\xi$ is an Irwin–Hall (CLT) surrogate for $\mathcal N(0,1)$: mean 0, variance $12\cdot\tfrac1{12}=1$, support $[-6,6]$. The $U_j$ come from a 64-bit LCG, $s\mapsto 6364136223846793005\,s+1442695040888963407 \pmod{2^{64}}$, seeded deterministically by $97t+1{,}000{,}003\,s+999{,}983\,a$, so **the whole run is a single deterministic draw**, not a distribution. Pulses $(A_p,t_p,\varsigma_p)$ are hand-specified in `PULSES` (15 of them).

What enters the mathematics: $y$, $\mu,\sigma,k,z_{thr}$, the catchment hyperedges, and the pulse table. What is stored but **never enters any computation**: `flow_liters`, `catchment_pop`, `lat`, `lon`, and the directed `site_flows_to` links (they are used only for reporting/queries).

---

## 2. Physics [PHYS]

The code documents the transport law it is modelled on (`analyte.rs`), and uses exactly one parameter from it ($k$). This section derives the law and its consequences so that the use of $k$, and the SHPINN in §7, are grounded.

### 2.1 Mass balance in a sewer reach

Take a reach of constant cross-section $A$, coordinate $x$, volumetric flow $Q=Av$. Let $c(x,t)$ be concentration (copies L⁻¹ or µg L⁻¹, **linear**). Conservation of mass on $[x,x+dx]$ with advective flux $Qc$, dispersive flux $-AD\,\partial_xc$, first-order loss $-kAc\,dx$ and source $Af\,dx$:

$$
A\,\partial_t c+\partial_x\!\big(Qc-AD\,\partial_xc\big)=-kAc+Af
\;\;\Longrightarrow\;\;
\boxed{\;\partial_t c+v\,\partial_x c=D\,\partial_{xx}c-k\,c+f(x,t)\;}
$$

This is the equation quoted in `analyte.rs`. $v$ mean velocity, $D$ effective axial dispersion, $k$ first-order decay (biological/chemical/hydrolytic), $f$ source (shedding, discharge).

### 2.2 Units caveat: the PDE is linear in $c$, not in $\log_{10}c$

The comment in `analyte.rs` labels $C$ as "log₁₀ copies/L". The equation above holds for linear concentration. With $\ell=\log_{10}c$ (so $c=e^{\ln10\,\ell}$):

$$
\partial_t\ell+v\,\partial_x\ell=D\,\partial_{xx}\ell+D\ln 10\,(\partial_x\ell)^2-\frac{k}{\ln10}+\frac{f}{c\,\ln 10}.
$$

Two consequences: (i) decay is a **constant drift** in log space, $-k/\ln10$ log₁₀ units per day (SARS-CoV-2, $k=0.5$: $0.217$ d⁻¹); (ii) dispersion picks up a nonlinear (Burgers/KPZ-type) term. A PINN must therefore either work in linear $c$ or use this transformed equation (§7).

### 2.3 Fundamental solution (proof)

**Proposition 2.1.** For an instantaneous unit-mass release at $x=0$, $t=0$ in an unbounded reach,
$$
G(x,t)=\frac{1}{\sqrt{4\pi Dt}}\exp\!\Big(-\frac{(x-vt)^2}{4Dt}\Big)e^{-kt}.
$$

*Proof.* Put $c=e^{-kt}w(\xi,t)$, $\xi=x-vt$. Then $c_t=-kc+e^{-kt}(w_t-vw_\xi)$, $c_x=e^{-kt}w_\xi$, $c_{xx}=e^{-kt}w_{\xi\xi}$. Substituting into $c_t+vc_x=Dc_{xx}-kc$ the $-kc$ terms cancel and the $vw_\xi$ terms cancel, leaving $w_t=Dw_{\xi\xi}$, the heat equation, whose point-source solution is the Gaussian; the initial condition $w(\xi,0)=\delta(\xi)$ gives the prefactor. $\square$

**Consequences.**

* Total mass decays exactly as $\int G\,dx=e^{-kt}$; half-life $t_{1/2}=\ln2/k$.
* Peak arrival at a station distance $L$ downstream: $t^*\approx L/v$, attenuated by $e^{-kL/v}$.
* Temporal width of the breakthrough curve: $\sigma_t\approx\sqrt{2DL/v^3}$ (spatial variance $2Dt$ at $t=L/v$, divided by $v^2$).
* For a general source, $c=G*f$ (space–time convolution, superposition). This linearity is what makes the inverse problem in §7 tractable.

### 2.4 Kinetic parameters used by the code ✔

Decay is the *only* physical parameter consumed. Values from `ANALYTE_CATALOG`, with derived quantities:

| Analyte | Cat. | $k$ (d⁻¹) | $t_{1/2}=\ln2/k$ (d) | log₁₀ slope $k/\ln10$ | raw $1-e^{-k}$ | α used |
|---|---|---|---|---|---|---|
| SARS-CoV-2 | P | 0.500 | 1.39 | 0.217 | 0.393 | 0.393 |
| Mpox | P | 0.450 | 1.54 | 0.195 | 0.362 | 0.362 |
| Vibrio cholerae | P | 0.400 | 1.73 | 0.174 | 0.330 | 0.330 |
| Influenza A | P | 0.550 | 1.26 | 0.239 | 0.423 | **0.400** (capped) |
| Poliovirus | P | 0.350 | 1.98 | 0.152 | 0.295 | 0.295 |
| Norovirus GII | P | 0.300 | 2.31 | 0.130 | 0.259 | 0.259 |
| Amoxicillin | R | 0.150 | 4.62 | 0.065 | 0.139 | 0.139 |
| Ciprofloxacin | R | 0.080 | 8.66 | 0.035 | 0.077 | **0.100** (floored) |
| AMR blaTEM | R | 0.200 | 3.47 | 0.087 | 0.181 | 0.181 |
| Fluoxetine | R | 0.050 | 13.9 | 0.022 | 0.049 | **0.100** |
| Fentanyl | I | 0.250 | 2.77 | 0.109 | 0.221 | 0.221 |
| Cocaine | I | 0.280 | 2.48 | 0.122 | 0.244 | 0.244 |
| Methamphetamine | I | 0.220 | 3.15 | 0.096 | 0.197 | 0.197 |
| MDMA | I | 0.200 | 3.47 | 0.087 | 0.181 | 0.181 |
| Lead (Pb) | T | 0.010 | 69.3 | 0.004 | 0.010 | **0.100** |
| PFAS (PFOA) | T | 0.005 | 139 | 0.002 | 0.005 | **0.100** |
| Trichloroethylene | T | 0.040 | 17.3 | 0.017 | 0.039 | **0.100** |
| Mercury (Hg) | T | 0.002 | 347 | 0.001 | 0.002 | **0.100** |

The clamp $[0.10,0.40]$ is inactive exactly for $k\in[-\ln0.9,\,-\ln0.6]=[0.1054,\,0.5108]$. Seven of the 18 analytes are clamped: Influenza A (cap at 0.40) and Ciprofloxacin, Fluoxetine, Pb, PFOA, TCE, Hg (floor at 0.10). The `lib.rs` table's "pathogen α range 0.26–0.39" should read 0.26–0.40.

### 2.5 Physical scale check

Sewer residence times are hours, so $k\tau\ll1$ over a reach (e.g. $\tau=6$ h, $k=0.5$: $e^{-0.125}=0.88$). The simulated 0.5–1 day lags between Belize City sites (`peak_day` 9 → 10 → 10.5) therefore encode **epidemic spread**, not sewer transit. The pulses are not solutions of the PDE; the simulator never integrates it.

---

## 3. Temporal detector: EWMA baseline + Welford spread [IMPL]

### 3.1 The estimators

For each key $(s,a)$ with observations $x_1,x_2,\dots$ (log₁₀ concentrations):

$$
\hat m_t=\alpha x_t+(1-\alpha)\hat m_{t-1},\qquad
\bar x_n=\bar x_{n-1}+\tfrac{x_n-\bar x_{n-1}}{n},\qquad
M_n=M_{n-1}+(x_n-\bar x_{n-1})(x_n-\bar x_n),
$$
$$
\hat s_n^2=\max\!\big(M_n/(n-1),\,10^{-6}\big),\qquad
z_t=\frac{x_t-\hat m_{t-1}}{\max(\hat s_{n},\,10^{-4})}.
$$

The z-score uses the **EWMA as centre** and the **Welford long-run variance as spread**, evaluated *before* the current observation is absorbed (so an observation never dilutes its own score). It is defined only once $n\ge7$ (`MIN_OBS`); otherwise $z=0$. An alert requires $z\ge z_{thr,a}$ (one-sided: drops never alert).

### 3.2 Welford exactness

**Lemma 3.1.** $M_n=\sum_{i\le n}(x_i-\bar x_n)^2$ exactly.

*Proof.* $\sum_{i\le n}(x_i-\bar x_n)^2=\sum_{i\le n}x_i^2-n\bar x_n^2$. Subtract the same identity at $n-1$: the difference is $x_n^2-n\bar x_n^2+(n-1)\bar x_{n-1}^2$. Using $\bar x_n=\bar x_{n-1}+\delta/n$, $\delta=x_n-\bar x_{n-1}$, this simplifies to $\delta\,(x_n-\bar x_n)$. $\square$

So $\hat s_n^2$ is the ordinary unbiased sample variance of *all* observations to date, including outbreak observations (this matters in §3.6).

### 3.3 Physics ↔ statistics: the α–k identity

The code justifies $\alpha=\operatorname{clamp}(1-e^{-k},0.10,0.40)$ by "a fraction $1-e^{-k}$ decays per day". The exact statement is stronger.

**Theorem 3.2 (ZOH identity).** Consider the first-order relaxation ODE $\dot{\hat m}=k\,(x-\hat m)$ driven by a signal $x$ held constant over each sampling interval $\Delta t=1$ d. Its exact solution over one interval is
$$
\hat m(t+\Delta t)=e^{-k\Delta t}\,\hat m(t)+(1-e^{-k\Delta t})\,x ,
$$
which is the EWMA recursion with $\alpha=1-e^{-k\Delta t}$. Equivalently the EWMA weight on the observation $j$ steps back is $\alpha(1-\alpha)^j=\alpha e^{-kj\Delta t}$: **the baseline forgets at exactly the analyte's physical decay rate**, and its half-life in observations equals $\ln2/k$ days.

*Proof.* Variation of constants: $\hat m(t+\Delta t)=e^{-k\Delta t}\hat m(t)+k\int_0^{\Delta t}e^{-k(\Delta t-\tau)}x\,d\tau$; with $x$ constant the integral is $x(1-e^{-k\Delta t})$. Since $\ln(1-\alpha)=-k\Delta t$, half-life in steps is $\ln\tfrac12/\ln(1-\alpha)=\ln2/(k\Delta t)$. $\square$

✔ Table in §2.4: for unclamped analytes the EWMA half-life equals $t_{1/2}$ (e.g. SARS-CoV-2 1.39 obs; Amoxicillin 4.62 obs). For the 7 clamped analytes the effective rate is $k_{\rm eff}=-\ln(1-\alpha)\in\{0.1054,0.5108\}$ and the EWMA half-life saturates at 6.58 obs (floor) or 1.36 obs (cap).

**Caveat (modelling, not mathematics).** The in-pipe decay acts over hours; the EWMA memory acts on day-to-day baseline drift. Theorem 3.2 shows the mapping is *self-consistent*, not that it is *derived* from the PDE. It is a design heuristic that gives slow-decay analytes slow baselines.

### 3.4 Null behaviour: the z-score is not a Gaussian z-score

Under a stationary null $x_t\sim\mathcal N(\mu,\sigma^2)$ i.i.d.:

* $\operatorname{Var}(x_t-\hat m_{t-1})=\sigma^2\big(1+\tfrac{\alpha}{2-\alpha}\big)=\tfrac{2}{2-\alpha}\sigma^2$ (steady state), so the numerator has s.d. $\sqrt{2/(2-\alpha)}\,\sigma$ (1.11σ at $\alpha=0.39$).
* The denominator $\hat s_n$ is a **random** estimate with only $\nu\approx n-1$ degrees of freedom (6 at the first evaluation).

Hence $z\approx\sqrt{2/(2-\alpha)}\;t_\nu$ (approximate: numerator and denominator are not independent because $\hat m$ and $\hat s$ share data). Student-$t$ tails with $\nu=6$–8 are far heavier than Gaussian:

| Quantity | Value |
|---|---|
| Nominal one-sided Gaussian tail at 2.5σ | 0.0062 |
| Approx. tail $P(z\ge2.5)$, $\alpha=0.39$, $\nu=6$ / 8 / 12 | 0.033 / 0.028 / 0.022 |
| Exact Monte-Carlo per-test rate with the code's semantics ✔ | pathogens ≈ 0.026–0.027; pharma 0.012–0.013; illicit ≈ 0.016; toxicants ≈ 0.0065 |

So "2.5σ" is a ~2.7 % per-test false-alarm rate, about 4.4× the Gaussian nominal. Over the run (144 series, evaluations on days 6–14):

* Expected null false alerts, exact code semantics, 40 000 Monte-Carlo replicates ✔: **21.5** across all 144 series (**≈19.3** across the 129 series that carry no injected pulse).
* Observed in the real run ✔: **26** alerts at no-pulse pairs, of 51 total. Same order of magnitude; the run is a single deterministic draw.

Multiplicity compounds this: 144 tests per day give an expected 1.5–3.9 null alerts per day after warm-up (day 6 is highest, 3.9, because $\nu$ is smallest).

### 3.5 Warm-up defect: the first observation is counted twice

`EwmaBaseline::ingest` builds `State::new(first)` (which sets $n=1$) **and then calls `update(first)` on the same value**, so after the first sample $n=2$. Consequences (✔ replica reproduces the binary only with this behaviour):

* `is_warm` ($n\ge7$) first holds on **day 6**, when only **6 distinct** observations (days 0–5) exist, not 7.
* Day 0 enters Welford twice, and the duplicate adds a zero-deviation term while raising the divisor. Monte-Carlo ✔: at day 6, $\mathbb E[\hat s^2]=0.952\,\sigma^2$ (≈5 % low) instead of $\sigma^2$, inflating $z$ by ≈2.5 %. This is a minor effect. (The day-6 count of pure-noise alerts, 8 observed vs 3.5 expected, is one deterministic draw; the expectation already includes the heavy $t_6$ tail and this bias.)

### 3.6 Tracking theory: why a slow outbreak can hide from its own detector

**Proposition 3.3 (ramp).** For a linear ramp $x_t=\mu+rt$ the EWMA lags by $L=r(1-\alpha)/\alpha$, so the steady-state innovation is
$$
x_t-\hat m_{t-1}=r+L=r/\alpha .
$$
*Proof.* $\hat m_t=x_t-L$ requires $L=(1-\alpha)(r+L)$, giving $L=r(1-\alpha)/\alpha$. $\square$

**Proposition 3.4 (self-masking).** Suppose $m$ of $n$ samples sit at a level $\Delta$ above the rest, with noise $\sigma$. Then
$$
\mathbb E[\hat s_n^2]=\frac{(n-2)\sigma^2+\tfrac{m(n-m)}{n}\Delta^2}{n-1}.
$$
*Proof.* The between-group decomposition gives $\sum(x_i-\bar x)^2=\sum_{\rm groups}\sum(x_i-\bar x_g)^2+\tfrac{m(n-m)}{n}\Delta^2$, and each within-group sum has expectation $(n_g-1)\sigma^2$. $\square$

The outbreak's own samples inflate the spread against which they are judged. A step is caught immediately ($m=1$: $z\approx\Delta/\sigma$), but a **gradual** rise loses on both fronts: the EWMA chases it (Prop. 3.3) and the variance grows (Prop. 3.4).

**Verified instance ✔ — `bcity_south`, SARS-CoV-2** (pulse amplitude 2.8 log₁₀, $\varsigma=2$ d, peak day 9; true noise $\sigma=0.22$; $\alpha=0.393$):

| Day | obs | EWMA before | innovation | innovation/σ_true | Welford s.d. | **z** |
|---|---|---|---|---|---|---|
| 6 | 5.004 | 4.101 | 0.903 | 4.1 | 0.378 | **2.39** |
| 7 | 5.216 | 4.456 | 0.760 | 3.5 | 0.538 | 1.41 |
| 8 | 6.073 | 4.755 | 1.318 | 6.0 | 0.647 | 2.03 |
| 9 | 6.486 | 5.274 | 1.212 | 5.5 | 0.866 | 1.40 |

The innovations are 3.5–6σ of the true noise, yet $z$ never reaches 2.5, because the estimated s.d. has grown 1.7–3.9× above $\sigma$. **This is variance contamination, not EWMA lag.** The same mechanism explains the missed Ciprofloxacin pulse at San Ignacio (§6).

---

## 4. Spectral hypergraph theory [IMPL]

### 4.1 The hypergraph and its Laplacian

`SewageNetwork::build` creates a weighted hypergraph $\mathcal H=(V,E,w)$:

* **Vertices:** the 8 monitoring sites.
* **Hyperedges:** each catchment with $\ge2$ members, weight 1, plus one **background** hyperedge containing all sites with weight $\varepsilon=0.05$ (`BACKGROUND_WEIGHT`, added to avoid isolated vertices).

With incidence $H\in\{0,1\}^{n\times m}$, $W=\operatorname{diag}(w_e)$, $D_e=\operatorname{diag}(\delta_e)$, $\delta_e=|e|$, $D_v=\operatorname{diag}(d_v)$, $d_v=\sum_e w_eH_{ve}$ (Zhou–Huang–Schölkopf 2006):

$$
\Theta=D_v^{-1/2}HWD_e^{-1}H^{\!\top}D_v^{-1/2},\qquad \Delta=I-\Theta .
$$

**Belize instance ✔.** Catchments with a single member (Orange Walk, San Ignacio, Dangriga) are skipped (`|members| < 2`), so

| $e$ | members | $w_e$ | $\delta_e$ |
|---|---|---|---|
| $e_1$ Belize City Trunk | north, south, wwtp | 1 | 3 |
| $e_2$ Belmopan Zone | core, industrial | 1 | 2 |
| $e_3$ background | all 8 sites | 0.05 | 8 |

Degrees: $d_v=\tfrac{21}{20}$ for the five clustered sites (1 + 0.05), $d_v=\tfrac1{20}$ for the three remote sites (background only). **The three remote sites are coupled to everything else only through the weak background edge.** The directed `flow_links` and the 6 `site_flows_to` relations play no role.

### 4.2 Fundamental theorem

**Theorem 4.1.** For $u\in\mathbb R^n$ put $x_v=u_v/\sqrt{d_v}$ and $\bar x_e=\delta_e^{-1}\sum_{v\in e}x_v$. Then

$$
\boxed{\;u^{\!\top}\Delta u=\sum_{e}w_e\sum_{v\in e}(x_v-\bar x_e)^2\;}
$$

and therefore (a) $\Delta\succeq0$; (b) $\Delta\preceq I$, i.e. $\operatorname{spec}(\Delta)\subset[0,1]$; (c) if $\mathcal H$ is connected, $\ker\Delta=\operatorname{span}(D_v^{1/2}\mathbf 1)$; (d) eigenvalue $1$ has multiplicity $\ge n-m$.

*Proof.* Since $\sum_ew_e\sum_{v\in e}x_v^2=\sum_vd_vx_v^2=\|u\|^2$ and $u^\top\Theta u=\sum_e\tfrac{w_e}{\delta_e}\big(\sum_{v\in e}x_v\big)^2$,
$$
u^\top\Delta u=\sum_ew_e\Big[\sum_{v\in e}x_v^2-\tfrac1{\delta_e}\Big(\sum_{v\in e}x_v\Big)^2\Big]=\sum_ew_e\sum_{v\in e}(x_v-\bar x_e)^2 .
$$
(a) is immediate. (b): $\Theta=BB^\top$ with $B=D_v^{-1/2}HW^{1/2}D_e^{-1/2}$, so $u^\top\Theta u\ge0$ and $u^\top\Delta u\le\|u\|^2$. (c): $u^\top\Delta u=0$ forces $x_v=\bar x_e$ on every hyperedge, hence $x$ constant on each hyperedge and, by connectivity, constant overall, i.e. $u\propto D_v^{1/2}\mathbf1$. (d): $\operatorname{rank}\Theta\le m$, so $\Theta$ has at least $n-m$ zero eigenvalues and $\Delta$ has eigenvalue 1 with that multiplicity. $\square$ ✔ (identity checked to $7\times10^{-15}$ on 2000 random fields).

**Interpretation.** $\Delta$ is the symmetrised generator of the two-step hypergraph random walk $P=D_v^{-1}HWD_e^{-1}H^\top$ (pick an incident hyperedge ∝ weight, then a uniform member): $\Delta=D_v^{1/2}(I-P)D_v^{-1/2}$. Each hyperedge is a **well-mixed compartment**. $u^\top\Delta u$ is the weighted within-catchment scatter of $x=D_v^{-1/2}u$. "Smooth" means "already mixed inside each catchment".

### 4.3 Exact spectrum and eigenmodes of the Belize network ✔

The non-zero spectrum of $\Theta$ equals that of the $3\times3$ Gram matrix
$$
B^\top B=\begin{pmatrix}\tfrac{20}{21}&0&\tfrac{\sqrt{30}}{42}\\[2pt]0&\tfrac{20}{21}&\tfrac{\sqrt5}{21}\\[2pt]\tfrac{\sqrt{30}}{42}&\tfrac{\sqrt5}{21}&\tfrac{17}{42}\end{pmatrix},\quad
\operatorname{spec}(B^\top B)=\{1,\tfrac{20}{21},\tfrac5{14}\}.
$$
Hence, with characteristic polynomial $\chi_\Delta(\lambda)=\lambda(\lambda-1)^5(14\lambda-9)(21\lambda-1)/294$ (sympy ✔):

| mode | $\lambda$ | eigenvector (unit; site groups N,S,W / C,I / O,SI,D) | meaning |
|---|---|---|---|
| $v_1$ | $0$ | $\propto D_v^{1/2}\mathbf1=(0.441^{\times5},\,0.0962^{\times3})$ | the "perfectly mixed" state |
| $v_2$ | $\tfrac1{21}\approx0.0476$ | $(0.365^{\times3},\,-0.548^{\times2},\,0^{\times3})$ | **Fiedler**: Belize-City trunk vs Belmopan contrast |
| $v_3$ | $\tfrac9{14}\approx0.643$ | $(0.0745^{\times5},\,-0.569^{\times3})$ | the five clustered sites vs the three remote sites |
| $v_{4..8}$ | $1$ (×5) | $\ker\Theta$: $\sum_{v\in e}x_v=0$ for all $e$ | within-group contrasts: 2 within the trunk, 1 within Belmopan, 2 among the remote trio |

Check: $\operatorname{tr}\Delta=0+\tfrac1{21}+\tfrac9{14}+5=\tfrac{239}{42}$ ✔. The binary prints `Fiedler λ₂=0.0476` ✔. $\lambda_2\to0$ as $\varepsilon\to0$ (the graph disconnects), which is exactly why the background edge exists.

### 4.4 The scores

For the field $u\in\mathbb R^8$ of one analyte-day:

$$
\mathrm{RQ}(u)=\frac{u^\top\Delta u}{2\,u^\top u},\qquad
\mathrm{HFF}(u)=1-\frac{\sum_{i\le2}\langle v_i,u\rangle^2}{u^\top u},\qquad
S=\operatorname{clamp}_{[0,1]}\big(0.6\,\mathrm{RQ}+0.4\,\mathrm{HFF}\big).
$$

The "smooth" set for HFF is $\{v_1,v_2\}$ (`k=2`): the kernel and the Fiedler mode. $S=0$ if $\|u\|^2<10^{-10}$.

### 4.5 Closed form and exact range

Let $p_i=\langle v_i,u\rangle^2/\|u\|^2$ (so $\sum_ip_i=1$) and $P_{\rm hi}=\sum_{i\ge4}p_i$.

**Theorem 4.2.**
$$
\boxed{\;S(u)=\tfrac1{70}\,p_2+\tfrac{83}{140}\,p_3+\tfrac{7}{10}\,P_{\rm hi}\;}
$$
*Proof.* $\mathrm{RQ}=\tfrac12\sum_i\lambda_ip_i=\tfrac12\big(\tfrac{1}{21}p_2+\tfrac9{14}p_3+P_{\rm hi}\big)$ and $\mathrm{HFF}=1-p_1-p_2=p_3+P_{\rm hi}$. Then $0.6\,\mathrm{RQ}$ contributes $\tfrac{0.3}{21}=\tfrac1{70}$ to $p_2$, $0.3\cdot\tfrac9{14}=\tfrac{27}{140}$ to $p_3$, $0.3$ to $P_{\rm hi}$; $0.4\,\mathrm{HFF}$ adds $0.4$ to $p_3$ and $P_{\rm hi}$. Summing: $\tfrac{27}{140}+\tfrac{56}{140}=\tfrac{83}{140}$ and $0.3+0.4=0.7$. $\square$ ✔ (max error $6\times10^{-16}$ over 2000 random fields).

**Corollary 4.3 (range).** $0\le S\le0.7$; $\mathrm{RQ}\in[0,\tfrac12]$, **not** $[0,1]$ as `spectral.rs` and the README state (that bound belongs to the standard normalised graph Laplacian, whose spectrum reaches 2). $S=0.7$ iff $u\in\ker\Theta$. The kernel component $p_1$ carries **zero** weight. The observed maximum over the run is $0.682$ ✔. The upgrade threshold 0.40 therefore sits at 57 % of the attainable range, not 40 %.

### 4.6 Null models: what the score reads on nothing

**Isotropic noise.** If $u\sim\mathcal N(0,\sigma^2I_8)$ then $\mathbb E[p_i]=1/8$ by rotation invariance, so
$$
\mathbb E[S]=\tfrac18\Big(\tfrac1{70}+\tfrac{83}{140}+5\cdot\tfrac7{10}\Big)=\tfrac{115}{224}\approx0.513,\qquad P(S>0.40)\approx0.80\ \text{✔ (MC)}.
$$
$S$ is a **high-pass** statistic: white noise scores above the alarm threshold.

**Constant field.** $u=c\mathbf 1$ has $p_1=\dfrac{(\sum_v\sqrt{d_v})^2}{n\sum_vd_v}=\dfrac{534+30\sqrt{21}}{864}\approx0.777$, $p_3=1-p_1$, so
$$
S(c\mathbf1)=\tfrac{83}{140}(1-p_1)\approx0.1321\quad\text{✔, independent of }c.
$$

**Level plus noise** $u=\mu\mathbf1+\sigma\varepsilon$ ✔ (MC, 5000 draws each):

| $\mu/\sigma$ | 0 | 0.5 | 1 | 2 | 3 | 5 | 10 | ∞ |
|---|---|---|---|---|---|---|---|---|
| mean $S$ | 0.512 | 0.449 | 0.334 | 0.212 | 0.172 | 0.146 | 0.136 | 0.132 |
| $P(S>0.40)$ | 0.80 | 0.64 | 0.32 | 0.02 | 0 | 0 | 0 | 0 |

### 4.7 The kernel mismatch and non-invariance

Theorem 4.1 shows the zero-energy state is $u\propto D_v^{1/2}\mathbf 1=(1.025^{\times5},\,0.224^{\times3})$, i.e. concentrations ∝ $\sqrt{d_v}$. The code feeds the **raw concentration** $u=y$ into $\Delta$, so the Dirichlet form measures scatter of $y_v/\sqrt{d_v}$, in which the remote sites are weighted $4.6\times$ the clustered ones. A perfectly uniform field is *not* smooth.

**Proposition 4.4.** $S(u+a\mathbf1)\ne S(u)$ in general; $S$ depends on the origin of the log scale. ✔ Example, a SARS-CoV-2-like field $u$ with entries 3.6–4.3: $S(u)=0.119$, $S(u-3)=0.091$ (a unit change L→mL), $S(u-3.8)=0.292$ (baseline-subtracted).

**Proposition 4.5 (sensitivity at a constant field).** At $u=3.8\cdot\mathbf1$ ✔: $\partial S/\partial u_v=-0.0126$ for each of the five clustered sites and $+0.0210$ for each remote site, with $5(-0.0126)+3(0.0210)=0$ as required by degree-0 homogeneity ($\nabla S\cdot u=0$). **Raising the concentration at any of the five clustered sites *lowers* $S$; raising a remote site raises it.**

This is what happens to the flagship event ✔:

| SARS-CoV-2 day | 5 | 8 | 9 | 10 |
|---|---|---|---|---|
| $S$ | 0.126 | **0.083** | **0.083** | **0.076** |

The outbreak sits on the high-degree trunk sites, which carry 0.441 of the unit kernel vector each; raising them increases $p_1$ and lowers $S$. The SARS-CoV-2 score falls during the outbreak, and never exceeds 0.137 in the run.

### 4.8 Consequence for the severity upgrade

Because $S$ is driven by $\mu/\sigma$ rather than anomaly, which analytes can ever be "upgraded" is a function of the catalog's log₁₀ level, not of the field's spatial structure ✔:

| Analyte | $\mu$ | $\sigma$ | $\lvert\mu\rvert/\sigma$ | mean $S$ (days 0–7) | max $S$ | rounds $S>0.4$ / 15 |
|---|---|---|---|---|---|---|
| SARS-CoV-2 | 3.80 | 0.22 | 17.3 | 0.127 | 0.137 | 0 |
| Mpox | 1.00 | 0.18 | 5.6 | 0.129 | 0.190 | 0 |
| Vibrio cholerae | 2.20 | 0.20 | 11.0 | 0.128 | 0.173 | 0 |
| Influenza A | 2.50 | 0.25 | 10.0 | 0.129 | 0.167 | 0 |
| Poliovirus | 0.50 | 0.15 | 3.3 | 0.177 | 0.260 | 0 |
| Norovirus GII | 3.20 | 0.28 | 11.4 | 0.147 | 0.207 | 0 |
| Amoxicillin | 1.20 | 0.18 | 6.7 | 0.136 | 0.173 | 0 |
| Ciprofloxacin | 1.00 | 0.16 | 6.2 | 0.188 | 0.269 | 0 |
| AMR blaTEM | 4.50 | 0.20 | 22.5 | 0.134 | 0.150 | 0 |
| Fluoxetine | 0.80 | 0.15 | 5.3 | 0.147 | 0.192 | 0 |
| Fentanyl | −0.50 | 0.30 | 1.7 | 0.196 | 0.624 | 4 |
| Cocaine | 0.20 | 0.25 | 0.8 | **0.456** | 0.682 | **10** |
| Methamphetamine | −0.20 | 0.28 | 0.7 | 0.376 | 0.599 | 6 |
| MDMA | −0.40 | 0.22 | 1.8 | 0.201 | 0.487 | 1 |
| Lead (Pb) | 0.50 | 0.12 | 4.2 | 0.156 | 0.505 | 3 |
| PFAS (PFOA) | 0.30 | 0.10 | 3.0 | 0.173 | 0.263 | 0 |
| Trichloroethylene | 0.20 | 0.15 | 1.3 | 0.244 | 0.337 | 0 |
| Mercury (Hg) | −0.20 | 0.12 | 1.7 | 0.199 | 0.653 | 5 |

29 of 270 rounds exceed 0.40; **all 29 belong to the low-$|\mu|/\sigma$ analytes** (illicit substances and metals). Cocaine averages 0.456 *before any outbreak*, i.e. above threshold on its own baseline, consistent with the $\mu/\sigma\lesssim1$ rows of §4.6. For **all six infectious pathogens the upgrade does not fire in this run** (max $S\le0.26$). Ten alerts carry $S>0.40$, all illicit-substance or metal alerts; in 8 of them the score changes the tier (e.g. the Day-7 Methamphetamine alert at $z=3.16$, AMBER→RED, is upgraded by the score alone; the two others were already CRITICAL).

---

## 5. Fusion, triage, and provenance [IMPL]

### 5.1 The severity map

Let $z_{thr}$ be the analyte's threshold (2.5 pathogens, 3.0 pharma/AMR, 2.8 illicit, 3.5 toxicants). Base tier, tested **in this order**:

$$
b(z)=\begin{cases}\text{CRITICAL}&z\ge5\\ \text{RED}&z\ge3.5\\ \text{AMBER}&z\ge z_{thr}\\ \text{GREEN}&\text{otherwise}\end{cases}
\qquad
\operatorname{sev}(z,S)=\begin{cases}\uparrow b(z)&S>0.40\\ b(z)&\text{otherwise}\end{cases}
$$

where $\uparrow$ maps AMBER→RED, RED→CRITICAL and leaves the rest fixed.

**Theorem 5.1.**
1. An event is emitted iff the baseline is warm and $z\ge z_{thr}$. $S$ can never create or suppress an alert; it only changes the tier of one that already exists.
2. $\operatorname{sev}$ is monotone non-decreasing in both $z$ and $S$ (with $S$ entering only through the indicator $S>0.40$).
3. $S$ raises the tier by at most one; CRITICAL is absorbing.
4. GREEN is never emitted as an event.
5. For toxicants ($z_{thr}=3.5$) the AMBER tier is unreachable: the test $z\ge3.5$ fires first, so every toxicant alert is at least RED. ✔ All 10 toxicant alerts in the run are RED or CRITICAL.

*Proof.* Direct from the code path in `observe_analyte` and `from_scores`; (5) because $b$ tests $3.5$ before $z_{thr}$. $\square$

### 5.2 Triage

`ww_alert_agent` serialises open alerts to JSON and pipes them to `agent_bridge.py`, which calls a model with a fixed rule prompt and a forced tool call, and falls back to the deterministic rules below if the SDK/API is unavailable. In this run the deterministic fallback executed ✔ (every decision carries a `Fallback:` reasoning string; the binary reported "API key present" because an *empty* `ANTHROPIC_API_KEY` is still `Some("")`, and no live model call succeeded). No claim in this document depends on live-model behaviour.

Deterministic fallback, as a function of the alert:

$$
\text{action}=\begin{cases}\text{Escalate}&\text{severity}\in\{\text{RED},\text{CRITICAL}\}\\ \text{Dismiss}&\text{AMBER}\wedge z<2.8\wedge S<0.15\\ \text{Confirm}&\text{otherwise}\end{cases}
$$

The model prompt adds: co-occurring analytes at one site are treated as a correlated source event; industrial toxicants at Dangriga/Belmopan-Industrial escalate; illicit AMBER with $z\ge3$ escalates. Every alert must receive a decision.

Note the interaction with §4: the Dismiss rule needs $S<0.15$, which for high-$|\mu|/\sigma$ analytes is *typical* (baseline $S\approx0.13$), so it operates mostly as a $z<2.8$ rule for those analytes, and almost never for cocaine/methamphetamine (baseline $S\approx0.4$).

### 5.3 Ontology and audit as structures

* **Ontology:** a typed property graph, 5 object types and 4 link types. Provenance is a composition of three total many-to-one maps, $\text{alert}\to\text{signal}\to\text{sample}\to\text{site}$, so every alert has a unique evidence chain (`EvidenceChain::build`).
* **Audit log:** an append-only sequence (a `Vec` behind a mutex) with atomic counter ids `aud_000001…` and UTC millisecond timestamps; export is NDJSON of `AuditEntry`. There is **no hash chain or signature**, so it is append-only by API, not tamper-evident by construction, despite "immutable"/"signed" in doc comments. The README's WHO-IHR/ECDC alignment is not implemented as schema mapping in code.

---

## 6. What the shipped run demonstrates ✔

Totals from the binary: **51 alerts** (24 AMBER, 18 RED, 9 CRITICAL); by category 20 pathogen / 9 pharma-AMR / 12 illicit / 10 toxicant. Of the 51, **25** are at (site, analyte) pairs carrying an injected pulse and **26** at pairs with none (expected ≈ 19.3 under the i.i.d. null, §3.4). No alert on a pulse pair occurs while the pulse contribution is below 0.3 log₁₀.

Response of each injected pulse (alert = day, $z$, tier):

| Site — analyte | Peak day | $A$ | $\varsigma$ | Alerts |
|---|---|---|---|---|
| bcity_south — SARS-CoV-2 | 9 | 2.8 | 2.0 | **none** |
| bcity_north — SARS-CoV-2 | 10 | 1.6 | 2.0 | d7 6.07 CRIT; d8 3.20 AMBER |
| bcity_wwtp — SARS-CoV-2 | 10.5 | 1.2 | 2.0 | d8 3.34 AMBER |
| belmopan_core — Mpox | 11 | 3.2 | 1.5 | d9 4.71 RED; d10 3.49 AMBER; d11 3.00 AMBER |
| orange_walk — Norovirus | 8.5 | 1.8 | 2.5 | d6 3.99 RED |
| bcity_south — Fentanyl | 9.5 | 2.2 | 1.5 | d8 7.02 CRIT |
| orange_walk — Cocaine | 10 | 1.9 | 1.0 | d9 3.57 CRIT; d10 3.57 CRIT |
| belmopan_core — Methamphetamine | 11.5 | 1.6 | 1.5 | d9 2.91 RED; d10 3.02 RED |
| belmopan_ind — Methamphetamine | 12 | 1.2 | 1.5 | d11 4.41 RED; d12 3.35 AMBER |
| belmopan_core — Amoxicillin | 9 | 1.4 | 2.0 | d7 3.06 AMBER; d8 4.21 RED |
| belmopan_core — AMR blaTEM | 10 | 0.8 | 2.5 | d9 4.26 RED |
| san_ignacio — Ciprofloxacin | 8 | 1.2 | 2.0 | **none** |
| dangriga — Lead | 10.5 | 2.0 | 1.5 | d8 3.95 RED; d9 5.86 CRIT; d10 5.39 CRIT |
| belmopan_ind — TCE | 11 | 1.8 | 1.5 | d10 3.90 RED; d11 3.72 RED |
| dangriga — Mercury | 10.5 | 1.5 | 2.0 | d8 4.08 CRIT; d9 4.63 CRIT; d10 3.54 CRIT |

Reading these against the theory:

* **Sharp, narrow pulses are caught early** (Fentanyl $\varsigma=1.5$, Lead, Mercury, Cocaine): a fast rise gives a large innovation before the variance inflates (Prop. 3.3–3.4).
* **The two misses are the slow, wide pulses** at high $\alpha^{-1}$: SARS-CoV-2 at the source site (§3.6) and Ciprofloxacin ($\alpha=0.10$ floor, $z_{thr}=3.0$, $\varsigma=2$).
* The SARS-CoV-2 cluster is first flagged at a *downstream secondary* site (`bcity_north`, day 7), whose Day-7 excess of 0.52 log₁₀ from the pulse is combined with a favourable noise draw.
* The README's "detects both events before simulated clinical presentation" is not testable: no clinical-presentation model exists in the code.
* Where the spectral upgrade fires (Mercury, Cocaine, Methamphetamine, Lead) it does so because $|\mu|/\sigma$ is small (§4.8), not because the outbreak is network-wide.

---

## 7. DSPINN: Spectral-Hypergraph Physics-Informed Neural Network [SPEC]

> **Status.** Nothing in this section exists in the archive. It is the formulation the schema is built to feed: `decay_rate_k` is exported per signal "for DSPINN PDE regression", `analyte.rs` names the PDE, and the illicit-substance category is described as "inverse source localisation". The proofs below are about this specification, not about running code.

### 7.1 Problem statement

Unknown: the concentration field $c_e(x,t)\ge0$ on every sewer reach $e$ of the directed flow graph $(V,\mathcal E)$ (the `site_flows_to` links), and, in the inverse setting, the source field $f$ (shedding or discharge) and possibly $k,D$. Data: sparse noisy site measurements $y_{s,a,t}=\log_{10}c(x_s,t)+\eta$, $\eta\sim\mathcal N(0,\sigma_a^2)$ — exactly the log-normal measurement model already implicit in the detector.

**Governing equations** (per reach, linear concentration):
$$
\mathcal R_e[c]:=\partial_tc+v_e\partial_xc-D_e\partial_{xx}c+k\,c-f=0,
$$
with junction conditions at each confluence $j$: mass conservation $\sum_{i\in\mathrm{in}(j)}Q_ic_i(L_i,t)=Q_{\rm out}\,c_{\rm out}(0,t)$ (well-mixed node, negligible dispersive flux), with $Q_{\rm out}=\sum_iQ_i$.

### 7.2 Network PINN and loss

A network $\ell_\theta(e,x,t)$ outputs $\log_{10}c$ (this enforces $c>0$). Residuals are evaluated on the linear field $c_\theta=10^{\ell_\theta}$ (or via the transformed equation of §2.2). Sources are parametrised per site, $f_\phi\in\mathbb R^n$.

$$
\boxed{\;
\mathcal L=\underbrace{\sum_{s,a,t}\frac{(\ell_\theta(x_s,t)-y_{s,a,t})^2}{\sigma_a^2}}_{\text{data (log-normal)}}
+\lambda_r\underbrace{\sum_e\!\iint|\mathcal R_e[c_\theta]|^2}_{\text{PDE residual}}
+\lambda_j\,\underbrace{\|\text{junction}\|^2}_{\text{network}}
+\lambda_0\,\underbrace{\|c_\theta(\cdot,0)-c_0\|^2}_{\text{IC}}
+\beta\,\underbrace{g^{\!\top}\Delta\,g}_{\text{hypergraph prior}}
+\gamma\|f_\phi\|_1\;}
$$

with $g=D_v^{1/2}f_\phi$ (the scaling that makes uniform mixing the zero-energy state, cf. §4.7). The kinetic constant enters as a fixed input read from the ontology property `decay_rate_k` (or as a trainable $k$ initialised there).

### 7.3 Stability of the PINN solution (theorem and proof)

**Theorem 7.1.** Let $c$ solve $\partial_tc+v\partial_xc-D\partial_{xx}c+kc=f$ on $(0,L)\times(0,T]$ with $v,D>0$, $k\ge0$ constant, and let $c_\theta$ be any sufficiently smooth approximation that matches $c$ on the boundary and at $t=0$ (hard constraints), with residual $R:=\partial_tc_\theta+v\partial_xc_\theta-D\partial_{xx}c_\theta+kc_\theta-f$. With $\kappa:=k+D\pi^2/L^2$,
$$
\|c_\theta(T)-c(T)\|_{L^2}^2\;\le\;\frac1\kappa\int_0^Te^{-\kappa(T-s)}\|R(s)\|_{L^2}^2\,ds\;\le\;\frac{1-e^{-\kappa T}}{\kappa^2}\,\sup_{s\le T}\|R(s)\|_{L^2}^2 .
$$

*Proof.* $e=c_\theta-c$ satisfies $e_t+ve_x-De_{xx}+ke=R$, $e(0,\cdot)=e(L,\cdot)=0$, $e(x,0)=0$. Multiply by $e$ and integrate over $(0,L)$:
$$
\tfrac12\tfrac{d}{dt}\|e\|^2+\underbrace{\tfrac v2\big[e^2\big]_0^L}_{=0}+D\|e_x\|^2+k\|e\|^2=(R,e).
$$
The Poincaré inequality on $H_0^1(0,L)$, $\|e_x\|^2\ge(\pi/L)^2\|e\|^2$, gives $D\|e_x\|^2+k\|e\|^2\ge\kappa\|e\|^2$. Young: $(R,e)\le\tfrac\kappa2\|e\|^2+\tfrac1{2\kappa}\|R\|^2$. Hence $\tfrac{d}{dt}\|e\|^2+\kappa\|e\|^2\le\|R\|^2/\kappa$, and Grönwall with $e(0)=0$ yields the first bound; integrating $e^{-\kappa(T-s)}$ gives the second. $\square$

**What it says physically.** The forward problem is *contractive* with rate $\kappa=k+D\pi^2/L^2$: errors are forgotten at rate $\kappa$, so the residual controls the solution error with constant $\sim1/\kappa^2$. High-$k$ analytes (pathogens, $k\approx0.3$–$0.55$ d⁻¹) are easy; persistent toxicants ($k\le0.04$ d⁻¹, PFOA 0.005) rely on dispersion alone, $\kappa\approx D\pi^2/L^2$, which is tiny on long reaches, so residual error accumulates over the horizon $T$ ($\tfrac{1-e^{-\kappa T}}{\kappa^2}\to T/\kappa$ as $\kappa T\to0$). The theory predicts toxicant regression to be the hardest case. (Data-error and network-generalisation terms add to this bound; see the general PINN error analyses of Mishra & Molinaro for the inverse setting.)

### 7.4 Inverse source localisation

By linearity (§2.3) the measured signal is $y=Af+\text{noise}$ with $A$ the space–time convolution against $G$ sampled at the stations. $A$ is a compact smoothing operator: its singular values decay (heuristically like $e^{-(D\omega^2+k)\tau}$ in Fourier picture), so the unregularised inverse is unbounded. The hypergraph term supplies the regulariser.

**Theorem 7.2.** Let $\Sigma$ be the noise covariance, $\beta,\gamma\ge0$, and
$$
J(f)=\|Af-y\|^2_{\Sigma^{-1}}+\beta\,f^\top D_v^{1/2}\Delta D_v^{1/2}f+\gamma\|f\|^2 .
$$
If $\gamma>0$, $J$ is strictly convex with unique minimiser $f^\star=\big(A^\top\Sigma^{-1}A+\beta D_v^{1/2}\Delta D_v^{1/2}+\gamma I\big)^{-1}A^\top\Sigma^{-1}y$. If $\gamma=0$, uniqueness holds iff $\ker A\cap\ker(D_v^{1/2}\Delta D_v^{1/2})=\{0\}$; for a connected hypergraph the second kernel is $\operatorname{span}(\mathbf 1)$ (Theorem 4.1(c) rescaled), so the prior alone cannot pin down the *level* of $f$, only its spatial contrast; the data must.

*Proof.* $J$ is a quadratic with Hessian $2(A^\top\Sigma^{-1}A+\beta M+\gamma I)$, $M=D_v^{1/2}\Delta D_v^{1/2}\succeq0$. It is positive definite when $\gamma>0$, giving strict convexity and the normal-equation solution; for $\gamma=0$ the Hessian is singular exactly on $\ker A\cap\ker M$. $\square$

With the $\ell_1$ term the problem stays convex (proximal/ADMM solvable) and promotes few active sources, matching the "locate the discharge" use case (e.g. the Lead/Mercury pulse at Dangriga, the Fentanyl pulse at Belize City South).

### 7.5 How the spectral score should sit downstream of a PINN

The defect in §4.7 disappears if $\Delta$ is applied to a **standardised, mixing-scaled residual** rather than raw log-concentration: $u_v=\sqrt{d_v}\,r_v$ with $r_v=(y_v-\hat c_v)/\sigma_v$ ($\hat c$ the PINN or baseline prediction). Then (i) $u$ has no large DC component (Prop. 4.4 no longer bites); (ii) the zero-energy state is the uniform residual; (iii) the null distribution of $S$ is that of §4.6's isotropic case only if residuals are white, so the alarm threshold must be set from the **empirical null** of $S$ (e.g. its 99th percentile on anomaly-free days), not fixed at 0.40. These are consequences of Theorem 4.1–4.2 and are not yet validated on data.

### 7.6 Identifiability of $k$

For two stations on one reach at separation $\ell$ with known $v$, a pulse's peak amplitude ratio is $\approx e^{-k\ell/v}\cdot(\text{dispersion factor})$ (Prop. 2.1), so $k=-\tfrac{v}{\ell}\ln(\text{ratio})+O(D)$ correction. Because $k\tau\ll1$ over sewer reaches (§2.5), the ratio is close to 1, so $k$ is weakly identifiable from spatial data alone; it is better constrained by holding-time experiments. This is why fixing $k$ from the ontology, as the schema does, is the sensible default.

---

## 8. What can and cannot be claimed

| Claim | Status | Basis |
|---|---|---|
| Baseline forgets at the analyte's physical decay rate (unclamped $k$) | **Proven** (Thm 3.2) | ZOH identity; ✔ table §2.4 |
| "2.5σ" corresponds to a 2.5σ Gaussian false-alarm rate | **False** | ≈2.7 % per test (Student-$t$); ✔ MC |
| Spectral score is a spatial-anomaly measure | **Not as implemented** | Not baseline-invariant (Prop 4.4); depends on $\lvert\mu\rvert/\sigma$; high-pass null 0.51 |
| $R(u)/2\in[0,1]$ | **False** | $\operatorname{spec}\Delta\subset[0,1]$ ⇒ $[0,\tfrac12]$; $S\le0.7$ |
| Network-spread upgrade flags outbreaks spreading through the trunk sewer | **Not supported** | SARS-CoV-2 $S$ *falls* to 0.076–0.083; never upgraded ✔ |
| Both scenario events detected | **Partly** | Mpox detected d9; SARS-CoV-2 source never alerted ✔ |
| The system contains a PINN | **False for this archive** | Only `decay_rate_k` + comments |
| The SHPINN specification is mathematically well-posed | **Proven for the stated setting** | Thm 7.1, 7.2 |
| Audit log is tamper-evident / signed | **False** | Plain in-memory `Vec`, no hash chain |
| Detection lead of 3–7 days before clinical symptoms | **Not testable here** | No clinical model in the code |

---

## Appendix A — Reproduction

```bash
tar -xzf ww_biosec_v0_3_final_tar.gz && cd ww_biosec
cargo build --release -p ww_runner            # Rust 1.75 (apt cargo/rustc 1.75 sufficed)
ANTHROPIC_API_KEY= ./target/release/ww_biosec  # 51 alerts, "Fiedler λ₂=0.0476"
```

The Python replica (numpy/sympy) mirrors `scenario.rs`, `baseline.rs`, `detector.rs`, `spectral.rs` and the Zhou Laplacian; its 51 alerts match the binary field-for-field.

## Appendix B — Discrepancy register and fix directions

| # | Where | Finding | Direction |
|---|---|---|---|
| 1 | `spectral.rs`, README | $\mathrm{RQ}\in[0,\tfrac12]$, not $[0,1]$; composite max 0.7 | Normalise by $\lambda_{\max}$ or recalibrate the 0.40 threshold |
| 2 | `spectral_score` input | Raw log₁₀ level fed to $\Delta$; kernel is $\sqrt d$, not $\mathbf1$ | Feed $\sqrt{d_v}\cdot$standardised residual (§7.5) |
| 3 | `detector.rs` | Fixed 0.40 threshold; null $S$ mean 0.51 for white residuals | Empirical-null quantile threshold |
| 4 | `baseline.rs::ingest` | First observation counted twice ($n=2$ after one sample) | Skip the extra `update` on the call that creates the state |
| 5 | `baseline.rs` | z-scores use $\hat s$ from all history incl. the outbreak (self-masking, §3.6); Gaussian threshold applied to $t$-distributed statistic | Robust spread (MAD), freeze variance during alert, or $t$-quantile thresholds |
| 6 | `baseline.rs` docs | "after flow normalisation" — `flow_liters` is stored but never used | Normalise by flow/population or fix the comment |
| 7 | `spectral.rs` | Directed `site_flows_to` not used; 3 sites couple only via the $\varepsilon=0.05$ edge | Build hyperedges from the flow graph if transport structure is intended |
| 8 | `lib.rs` (detection) | Pathogen α range 0.26–0.39 should be 0.26–0.40 | Doc fix |
| 9 | `analyte.rs` | PDE stated for "log₁₀ concentration"; it holds for linear concentration (§2.2) | Doc fix |
| 10 | README | "Detects both events…", IHR/ECDC alignment, "signed" audit records not supported by code | Reword or implement |

## Appendix C — v0.4 changes and measured effect

All numbers below are from the real binary (`ANTHROPIC_API_KEY= ./target/release/ww_biosec`) unless marked MC (Monte-Carlo of the detector recursion under an i.i.d. Gaussian null).

### C.1 What changed

| # | Register item | Change in v0.4 |
|---|---|---|
| 1 | Score range | Rayleigh quotient normalised by $\lambda_{\max}$ (=1 here): $\mathrm{RQ},S\in[0,1]$ |
| 2 | Raw-level input | Network score is applied to the mixing-scaled standardised residual $u_v=\sqrt{d_v}\,\mathrm{clamp}(z_v,\pm6)$ (`spectral_score_residual`); uniform residual is exactly zero-energy (unit-tested) |
| 3 | Fixed 0.40 | Upgrade threshold = empirical null quantile (q95 of i.i.d. $N(0,1)$ residuals, deterministic LCG): **0.966** on the Belize network |
| 4 | Double count | First observation counted once; warm-up = 7 prior observations (first possible alert: day 7) |
| 5 | Self-masking | `ingest_robust`: EWMA sees deviations winsorised to $\pm\tfrac12 z_{thr}s$; Welford variance excludes observations beyond $1.5\,z_{thr}s$ (active from the 5th observation) |
| 6, 8, 9, 10 | Docs | flow-normalisation comment, α range, log-vs-linear PDE note, README claims corrected |
| 7 | Flow topology | **Not changed** (design decision: the hypergraph is still built from catchments only) |
| — | SHPINN | New crate `ww_shpinn` (linear-feature PINN forward solver; hypergraph-regularised inverse source; Theorem 7.1/7.2 constants), 7 unit tests, runner demo |

### C.2 Parameter choice for the robust update (MC + replica grid)

Robustness parameters were chosen on the *scenario replica* and the *null Monte-Carlo* together; excluding outliers from the variance too aggressively raises false alarms (variance is estimated from a truncated sample).

| EWMA clip $\times z_{thr}$ | Variance-exclusion $\times z_{thr}$ | pulse pairs hit /15 | alerts on no-pulse pairs | MC expected null alerts (144 series) |
|---|---|---|---|---|
| — (v0.3, double-count) | — | 13 | 26 | 21.5 |
| 0.5 | 1.0 | 14 | 32 | 29.3 |
| **0.5** | **1.5 (chosen)** | **14** | **19** | **20.3** |
| 0.5 | 2.0 | 13 | 18 | 17.8 |
| 0.5 | 3.0 | 13 | 17 | 16.6 |
| 1.0 | 1.5 | 13 | 17 | 20.6 |

The grid is tuned on one deterministic 15-day scenario (a single draw); treat the chosen setting as a sensible default, not a validated optimum.

### C.3 Before / after on the shipped scenario ✔

| Metric | v0.3 | v0.4 |
|---|---|---|
| Alerts (AMBER / RED / CRITICAL) | 51 (24 / 18 / 9) | 62 (23 / 19 / 20) |
| At pulse pairs / at no-pulse pairs | 25 / 26 | **43 / 19** |
| Pulse pairs alerted (of 15) | 13 | **14** (Ciprofloxacin at San Ignacio still missed) |
| SARS-CoV-2 at the source site `bcity_south` | never alerted | first alert day 8 (z=2.56, AMBER), peak day 9 |
| Norovirus at `orange_walk` | day 6 only | day 7 (warm-up now ends at day 7) |
| Spectral-score tier changes | 8 (10 alerts > 0.40, all low $\lvert\mu\rvert/\sigma$ analytes) | 0 (no alert exceeds the calibrated 0.966) |

### C.4 Honest limitations of v0.4

* **The network score no longer upgrades anything.** With a correctly calibrated null the residual-field roughness of an isolated outlier is not distinguishable from that of noise: the statistic is high-pass and shape-only. The v0.3 upgrades were driven by $\lvert\mu\rvert/\sigma$ (Prop. 4.4), not by spatial structure. A discriminating network layer needs a different statistic (e.g. corroboration of an alert across sites in the same catchment); that is future work and is not claimed here.
* False-alarm rate remains a Student-$t$ phenomenon (≈2–3 % per test for pathogens); multiplicity over 144 series still yields ≈20 expected null alerts per 15 days.
* `ww_shpinn` solves the **linear** ADR equation on one reach with Dirichlet data; it does not yet handle junction networks, log-space measurements, or learn $k$. Its forward error against the analytic Gaussian solution is 0.004–0.005 (peak 1.0) for $k\in\{0.5,0.25,0.15,0.01\}$ ✔. Theorem 7.1's hypotheses (hard-constrained boundary/initial data) are *not* met by the soft-constrained solver, so `stability_bound` is provided as the reference constant, not as a certificate for `PielmSolution`.
