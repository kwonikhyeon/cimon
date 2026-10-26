-- cimon job store, schema 1 — design §13 and review-ui §11.

CREATE TABLE IF NOT EXISTS jobs (
  id               INTEGER PRIMARY KEY,
  tool             TEXT NOT NULL,
  project          TEXT NOT NULL,
  project_key      TEXT NOT NULL,
  ref              TEXT,
  commit_sha       TEXT,
  siblings_json    TEXT,
  recipe_source    TEXT,
  recipe_ref       TEXT NOT NULL DEFAULT 'main',
  recipe_commit    TEXT,
  recipe_sha256    TEXT,
  trial            INTEGER NOT NULL DEFAULT 0,
  request_json     TEXT NOT NULL,
  plan_json        TEXT,
  merge_key        TEXT,
  merged_into      INTEGER REFERENCES jobs(id),
  state            TEXT NOT NULL,
  verdict          TEXT,
  failure_class    TEXT,
  reason_code      TEXT,
  status_message   TEXT,
  summary          TEXT,
  warnings_json    TEXT,
  result_json      TEXT,
  created_by       TEXT NOT NULL,
  cancel_requested_by TEXT,
  workspace_mode   TEXT,
  workspace_slot   INTEGER,
  fixture_state    TEXT,
  created_at       TEXT NOT NULL,
  ready_at         TEXT,
  started_at       TEXT,
  finished_at      TEXT,
  last_updated_at  TEXT NOT NULL,
  expires_at       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS jobs_state ON jobs(state, created_at);
CREATE INDEX IF NOT EXISTS jobs_merge ON jobs(merge_key) WHERE state IN ('ready','running');
CREATE INDEX IF NOT EXISTS jobs_trial ON jobs(project, recipe_ref, recipe_commit) WHERE trial = 1;

CREATE TABLE IF NOT EXISTS tasks (
  task_id         TEXT PRIMARY KEY,
  job_id          INTEGER NOT NULL REFERENCES jobs(id),
  origin_job_id   INTEGER NOT NULL,
  requested_by    TEXT NOT NULL,
  client_info     TEXT,
  via_task        INTEGER NOT NULL,
  created_at      TEXT NOT NULL,
  last_polled_at  TEXT,
  detached_at     TEXT,
  detached_reason TEXT
);
CREATE INDEX IF NOT EXISTS tasks_job ON tasks(job_id);

CREATE TABLE IF NOT EXISTS steps (
  job_id        INTEGER NOT NULL REFERENCES jobs(id),
  seq           INTEGER NOT NULL,
  step_id       TEXT NOT NULL,
  kind          TEXT NOT NULL,
  result        TEXT NOT NULL,
  failure_class TEXT,
  reason        TEXT,
  exit_code     INTEGER,
  started_at    TEXT,
  finished_at   TEXT,
  excerpt       TEXT,
  judge_json    TEXT,
  PRIMARY KEY (job_id, seq)
);

CREATE TABLE IF NOT EXISTS excluded (
  job_id  INTEGER NOT NULL REFERENCES jobs(id),
  step_id TEXT NOT NULL,
  reason  TEXT NOT NULL,
  PRIMARY KEY (job_id, step_id)
);

CREATE TABLE IF NOT EXISTS artifacts (
  job_id       INTEGER NOT NULL REFERENCES jobs(id),
  name         TEXT NOT NULL,
  path         TEXT NOT NULL,
  bytes        INTEGER NOT NULL,
  sha256       TEXT NOT NULL,
  inspect_json TEXT,
  created_at   TEXT NOT NULL,
  expires_at   TEXT NOT NULL,
  deleted_at   TEXT,
  PRIMARY KEY (job_id, name)
);

CREATE TABLE IF NOT EXISTS toolchain_installs (
  kind          TEXT NOT NULL,
  version       TEXT NOT NULL,
  entry_sha256  TEXT NOT NULL,
  root          TEXT NOT NULL,
  checksum      TEXT,
  source_ref    TEXT,
  installed_by  TEXT NOT NULL,
  installed_at  TEXT NOT NULL,
  last_used_at  TEXT NOT NULL,
  PRIMARY KEY (kind, version, entry_sha256)
);

CREATE TABLE IF NOT EXISTS job_toolchains (
  job_id        INTEGER NOT NULL REFERENCES jobs(id),
  kind          TEXT NOT NULL,
  version       TEXT NOT NULL,
  entry_sha256  TEXT NOT NULL,
  probe         TEXT,
  PRIMARY KEY (job_id, kind)
);

CREATE TABLE IF NOT EXISTS warm_slots (
  project      TEXT NOT NULL,
  slot         INTEGER NOT NULL,
  last_ref     TEXT,
  last_commit  TEXT,
  last_used_at TEXT,
  PRIMARY KEY (project, slot)
);

CREATE TABLE IF NOT EXISTS warm_state (
  project         TEXT NOT NULL,
  slot            INTEGER NOT NULL,
  step_id         TEXT NOT NULL,
  last_ok_commit  TEXT,
  inputs_sha256   TEXT,
  toolchains_hash TEXT,
  def_sha256      TEXT,
  at              TEXT NOT NULL,
  PRIMARY KEY (project, slot, step_id)
);

CREATE TABLE IF NOT EXISTS proposals (
  id              INTEGER PRIMARY KEY,
  branch          TEXT NOT NULL,
  title           TEXT NOT NULL,
  description     TEXT,
  author          TEXT NOT NULL,
  status          TEXT NOT NULL,
  base_commit     TEXT NOT NULL,
  created_at      TEXT NOT NULL,
  updated_at      TEXT NOT NULL,
  promoted_commit TEXT,
  promoted_by     TEXT,
  promoted_at     TEXT,
  override_reason TEXT,
  closed_by       TEXT,
  closed_at       TEXT,
  close_reason    TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS proposals_open_branch ON proposals(branch) WHERE status = 'open';

CREATE TABLE IF NOT EXISTS proposal_comments (
  id          INTEGER PRIMARY KEY,
  proposal_id INTEGER NOT NULL REFERENCES proposals(id),
  author      TEXT NOT NULL,
  body        TEXT NOT NULL,
  head_commit TEXT NOT NULL,
  created_at  TEXT NOT NULL,
  deleted_at  TEXT
);

CREATE TABLE IF NOT EXISTS backups (
  kind          TEXT NOT NULL,
  at            TEXT NOT NULL,
  path          TEXT NOT NULL,
  offsite_ok_at TEXT,
  error         TEXT,
  PRIMARY KEY (kind, at)
);

CREATE TABLE IF NOT EXISTS users (
  name          TEXT PRIMARY KEY,
  token_sha256  TEXT NOT NULL UNIQUE,
  role          TEXT NOT NULL DEFAULT 'member',
  created_at    TEXT NOT NULL,
  disabled_at   TEXT
);

CREATE TABLE IF NOT EXISTS audit (
  id            INTEGER PRIMARY KEY,
  at            TEXT NOT NULL,
  user          TEXT NOT NULL,
  action        TEXT NOT NULL,
  job_id        INTEGER,
  proposal_id   INTEGER,
  recipe_commit TEXT,
  tailnet_login TEXT,
  detail        TEXT
);
CREATE INDEX IF NOT EXISTS audit_at ON audit(at);

CREATE TABLE IF NOT EXISTS web_sessions (
  id_sha256    TEXT PRIMARY KEY,
  user         TEXT NOT NULL,
  csrf         TEXT NOT NULL,
  created_at   TEXT NOT NULL,
  expires_at   TEXT NOT NULL,
  last_seen_at TEXT,
  revoked_at   TEXT
);

-- one-time codes that create a user on a team member's PC (cimon invite → /connect)
CREATE TABLE IF NOT EXISTS invites (
  code_sha256    TEXT PRIMARY KEY,
  name           TEXT NOT NULL,
  role           TEXT NOT NULL,
  created_by     TEXT NOT NULL,
  created_at     TEXT NOT NULL,
  expires_at     TEXT NOT NULL,
  redeemed_at    TEXT,
  redeemed_login TEXT
);

CREATE TABLE IF NOT EXISTS login_codes (
  code_sha256 TEXT PRIMARY KEY,
  user        TEXT NOT NULL,
  created_at  TEXT NOT NULL,
  expires_at  TEXT NOT NULL,
  used_at     TEXT
);
