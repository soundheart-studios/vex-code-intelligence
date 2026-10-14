SELECT a FROM events WHERE ts > 0;
INSERT INTO log SELECT * FROM events;
