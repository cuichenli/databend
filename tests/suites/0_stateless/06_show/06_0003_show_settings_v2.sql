SET enable_planner_v2=1;
SET max_threads=11;
SET unknown_settings=11; -- {ErrorCode 2801}
SHOW SETTINGS;
SHOW SETTINGS LIKE 'enable%';
SET enable_planner_v2=0;
