-- ww_biosec relational store.
--
-- Postgres is the system of record. The in-memory EWMA detector is a pure,
-- deterministic function of the ordered `observations` history and is rebuilt
-- from it at startup (see engine.rs), so nothing detector-related is stored
-- here beyond the observations themselves.

CREATE TABLE analytes (
    name           text PRIMARY KEY,
    category       text NOT NULL,
    target_marker  text NOT NULL,
    method         text NOT NULL,
    baseline_log   double precision NOT NULL,
    noise_std      double precision NOT NULL,
    decay_rate_k   double precision NOT NULL,
    z_threshold    double precision NOT NULL,
    ewma_alpha     double precision NOT NULL
);

CREATE TABLE monitoring_sites (
    site_id     text PRIMARY KEY CHECK (length(site_id) BETWEEN 1 AND 200),
    name        text,
    region      text,
    created_at  timestamptz NOT NULL DEFAULT now()
);

-- Catchment zones = hyperedges of the spectral model.
CREATE TABLE catchments (
    name text PRIMARY KEY CHECK (length(name) BETWEEN 1 AND 200)
);

CREATE TABLE catchment_members (
    catchment text NOT NULL REFERENCES catchments(name) ON DELETE CASCADE,
    site_id   text NOT NULL REFERENCES monitoring_sites(site_id) ON DELETE CASCADE,
    PRIMARY KEY (catchment, site_id)
);
CREATE INDEX catchment_members_site_idx ON catchment_members (site_id);

-- One detection round = one analyte on one date across the sites that reported.
CREATE TABLE rounds (
    round_id         bigserial PRIMARY KEY,
    analyte          text NOT NULL REFERENCES analytes(name),
    observed_on      date NOT NULL,
    n_observations   integer NOT NULL,
    n_warm           integer NOT NULL,
    spectral_score   double precision NOT NULL,
    spread_threshold double precision NOT NULL,
    n_alerts         integer NOT NULL,
    source           text NOT NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    UNIQUE (analyte, observed_on)
);

CREATE TABLE observations (
    obs_id       bigserial PRIMARY KEY,
    round_id     bigint NOT NULL REFERENCES rounds(round_id),
    site_id      text NOT NULL REFERENCES monitoring_sites(site_id),
    analyte      text NOT NULL REFERENCES analytes(name),
    observed_on  date NOT NULL,
    log10_conc   double precision NOT NULL
                 CHECK (log10_conc <> 'NaN'::float8 AND log10_conc BETWEEN -30 AND 30),
    n_samples    integer NOT NULL DEFAULT 1 CHECK (n_samples >= 1),
    source       text NOT NULL,
    ingested_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (site_id, analyte, observed_on)
);
CREATE INDEX observations_round_idx ON observations (round_id);
CREATE INDEX observations_series_idx ON observations (analyte, site_id, observed_on);

CREATE TABLE alerts (
    alert_id        bigserial PRIMARY KEY,
    obs_id          bigint NOT NULL UNIQUE REFERENCES observations(obs_id),
    round_id        bigint NOT NULL REFERENCES rounds(round_id),
    site_id         text NOT NULL,
    analyte         text NOT NULL,
    observed_on     date NOT NULL,
    log10_conc      double precision NOT NULL,
    ewma            double precision NOT NULL,
    z_score         double precision NOT NULL,
    spectral_score  double precision NOT NULL,
    severity        text NOT NULL CHECK (severity IN ('AMBER','RED','CRITICAL')),
    n_obs           integer NOT NULL,
    alpha           double precision NOT NULL,
    status          text NOT NULL DEFAULT 'OPEN'
                    CHECK (status IN ('OPEN','CONFIRMED','DISMISSED','ESCALATED')),
    analyst_notes   text NOT NULL DEFAULT '',
    reviewed_by     text,
    reviewed_at     timestamptz,
    created_at      timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX alerts_status_sev_idx ON alerts (status, severity);
CREATE INDEX alerts_site_idx ON alerts (site_id, observed_on);

-- Append-only audit trail, enforced by the database rather than by API
-- convention. (Not hash-chained or signed.)
CREATE TABLE audit_log (
    audit_id   bigserial PRIMARY KEY,
    ts         timestamptz NOT NULL DEFAULT clock_timestamp(),
    actor      text NOT NULL,
    action     text NOT NULL,
    target_id  text NOT NULL,
    details    text NOT NULL DEFAULT ''
);
CREATE INDEX audit_log_target_idx ON audit_log (target_id);

CREATE FUNCTION audit_log_reject_mutation() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'audit_log is append-only (% rejected)', TG_OP
        USING ERRCODE = 'restrict_violation';
END;
$$;

CREATE TRIGGER audit_log_no_update_delete
    BEFORE UPDATE OR DELETE ON audit_log
    FOR EACH ROW EXECUTE FUNCTION audit_log_reject_mutation();

CREATE TRIGGER audit_log_no_truncate
    BEFORE TRUNCATE ON audit_log
    FOR EACH STATEMENT EXECUTE FUNCTION audit_log_reject_mutation();
