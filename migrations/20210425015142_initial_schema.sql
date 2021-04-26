-- Add migration script here
CREATE TABLE IF NOT EXISTS snapshots (key VARCHAR PRIMARY KEY, value INT8 NOT NULL);