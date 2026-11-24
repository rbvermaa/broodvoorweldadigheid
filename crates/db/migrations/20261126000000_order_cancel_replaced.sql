ALTER TABLE "order" DROP CONSTRAINT order_cancel_reason_check;
ALTER TABLE "order" ADD CONSTRAINT order_cancel_reason_check CHECK (
    cancel_reason IN ('hold_expired', 'staff', 'customer', 'day_cancelled', 'replaced_by_customer')
);
