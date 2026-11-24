# 0005 BTW rounded per line

Status: accepted, 2026-11-24

Prices include BTW. Each line's gross (unit price × quantity) is split into net and BTW
with `VatRate::vat_part`, rounded half-up to whole cents. Totals per rate are the sums of
those line splits: rate net is the sum of line nets, rate BTW is the sum of line BTW.
Dutch practice allows rounding BTW per line.

Every document then adds up exactly: an order confirmation, the net per line on the
B2B invoice overview (F11a), and the sales report per rate (F11) all show the same
cents, whether the report sums all lines or the totals of each document
(`VatTotals::checked_add`).

A refund is a negative line, so it carries exactly the BTW of the line it reverses.
How a partial refund by amount on a mixed-rate order is split is an open question.
