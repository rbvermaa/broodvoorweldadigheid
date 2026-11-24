-- The customer's link to an order's status page after paying (F5). Random, so it cannot
-- be guessed from the sequential order number. gen_random_uuid() uses the server's strong
-- random source and is evaluated per existing row.
ALTER TABLE "order" ADD COLUMN status_token uuid NOT NULL DEFAULT gen_random_uuid();
ALTER TABLE "order" ADD CONSTRAINT order_status_token_key UNIQUE (status_token);
