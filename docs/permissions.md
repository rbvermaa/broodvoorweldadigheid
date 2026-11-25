# Permissions

Generated from `crates/auth/src/role.rs` by `make permissions-doc`; do not edit by hand.
Roles, sign-in and how routes check a permission: ADR 0010.

| Permission | What it covers | owner | baker | counter |
|---|---|---|---|---|
| CloseBakeDay | Sick-day cancel (F2), including that day's refunds (ADR 0028). | ✓ | | |
| Costing | Ingredient prices, standard and batch cost, cost variance (F16). | ✓ | | |
| EditAllergens | Allergens, ingredient text, nutrition approval. | ✓ | ✓ | |
| EditOrders | Moving an order to another pickup slot, cancelling an unpaid order (F6), placing an order on account (F11a). | ✓ | ✓ | |
| Invoicing | Business accounts, the invoice overview, marking orders invoiced (F11a). | ✓ | | |
| ManageBakeDays | Bake days, cutoffs, pickup slots, capacity. | ✓ | ✓ | |
| ManageCharities | Charities and payouts. | ✓ | | |
| ManageStaff | Adding staff and changing roles. | ✓ | | |
| PickupList | Pickup list, marking orders picked up. | ✓ | ✓ | ✓ |
| Polls | Bread votes: create, change, see results and wishes (F9b, ADR 0039). | ✓ | ✓ | |
| Privacy | Exporting and erasing a customer's data on their request (F12, ADR 0031). | ✓ | | |
| Production | Production list, recipes, batches, labels. | ✓ | ✓ | |
| Refund | Cancelling a paid order with its refund, retrying a refund (F6, ADR 0028). | ✓ | ✓ | |
| Settings | Company settings, such as switching the public donation page on (F11d, ADR 0040). | ✓ | | |
| ViewOrders | Order list and order details. | ✓ | ✓ | ✓ |
| ViewReports | Sales, donation and contribution reports (F11c, F11e). | ✓ | | |
