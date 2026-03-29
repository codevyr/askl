ALTER TABLE index.symbols ADD CONSTRAINT symbols_name_project_key UNIQUE (name, project_id);
