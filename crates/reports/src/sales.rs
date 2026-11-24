//! Sales per BTW rate per day, month or quarter, and pay-what-you-want prices (F11).

use std::collections::BTreeMap;

use domain::{Money, ProductId, VatRate, VatTotals};
use i18n::Translator;
use orders::OrderNumber;
use sqlx::PgConnection;

use crate::csv_out::{self, Csv};
use crate::facts::{self, OrderFact, Selection};
use crate::{Error, Granularity, Period};

/// Sales in one day, month or quarter. Each row's net and BTW are sums of per-line
/// splits (ADR 0005); refunds are negative lines (ADR 0017).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SalesRow {
    pub period: Period,
    pub totals: VatTotals,
}

/// Pay-what-you-want lines of one product.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayWhatYouWantRow {
    pub product_id: ProductId,
    pub product_name: String,
    pub pieces: u64,
    pub paid: Money,
    pub suggested: Money,
}

impl PayWhatYouWantRow {
    /// Average price paid per piece, rounded half-up to whole cents.
    #[must_use]
    pub fn average_paid(&self) -> Money {
        average(self.paid, self.pieces)
    }

    /// Average suggested price per piece, rounded half-up to whole cents.
    #[must_use]
    pub fn average_suggested(&self) -> Money {
        average(self.suggested, self.pieces)
    }
}

fn average(total: Money, pieces: u64) -> Money {
    if pieces == 0 {
        return Money::ZERO;
    }
    let total = i128::from(total.cents());
    let pieces = i128::from(pieces);
    let rounded = (2 * total.abs() + pieces) / (2 * pieces) * total.signum();
    Money::from_cents(i64::try_from(rounded).unwrap_or(i64::MAX))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SalesReport {
    pub period: Period,
    pub granularity: Granularity,
    /// Only days, months or quarters with sales, oldest first.
    pub rows: Vec<SalesRow>,
    pub total: VatTotals,
    /// Orders with a partial refund over both rates, split by estimate (ADR 0017).
    pub estimated_refunds: Vec<OrderNumber>,
    pub pay_what_you_want: Vec<PayWhatYouWantRow>,
}

/// Sales of orders sold in `period` (paid, or created when on account), in the bakery's
/// time zone, including drops without a charity.
///
/// # Errors
/// [`Error::Db`], [`Error::Corrupt`] or [`Error::Money`].
pub async fn report(
    conn: &mut PgConnection,
    period: Period,
    granularity: Granularity,
) -> Result<SalesReport, Error> {
    let (start, end) = period.instants()?;
    let orders = facts::orders(conn, Selection::SoldBetween(start, end)).await?;
    build(&orders, period, granularity)
}

pub(crate) fn build(
    orders: &[OrderFact],
    period: Period,
    granularity: Granularity,
) -> Result<SalesReport, Error> {
    let mut buckets: BTreeMap<jiff::civil::Date, (Period, VatTotals)> = BTreeMap::new();
    let mut total = VatTotals::default();
    let mut estimated_refunds = Vec::new();
    let mut pwyw: BTreeMap<ProductId, PayWhatYouWantRow> = BTreeMap::new();
    for order in orders {
        let day = order.sold_on();
        if !period.contains(day) {
            continue;
        }
        let (lines, split) = order.net_lines()?;
        if split.estimated {
            estimated_refunds.push(order.number);
        }
        let totals = VatTotals::from_lines(&lines)?;
        let bucket = granularity.bucket(day, &period);
        let entry = buckets
            .entry(bucket.from())
            .or_insert((bucket, VatTotals::default()));
        entry.1 = entry.1.checked_add(&totals)?;
        total = total.checked_add(&totals)?;
        for line in &order.lines {
            let Some(suggested) = line.suggested_price else {
                continue;
            };
            let row = pwyw
                .entry(line.product_id)
                .or_insert_with(|| PayWhatYouWantRow {
                    product_id: line.product_id,
                    product_name: line.product_name.clone(),
                    pieces: 0,
                    paid: Money::ZERO,
                    suggested: Money::ZERO,
                });
            row.pieces += u64::from(line.qty);
            row.paid = row.paid.checked_add(line.line().total()?)?;
            row.suggested = row
                .suggested
                .checked_add(suggested.checked_mul(line.qty)?)?;
        }
    }
    Ok(SalesReport {
        period,
        granularity,
        rows: buckets
            .into_values()
            .map(|(period, totals)| SalesRow { period, totals })
            .collect(),
        total,
        estimated_refunds,
        pay_what_you_want: pwyw.into_values().collect(),
    })
}

/// One row per period and BTW rate, then the totals per rate.
///
/// # Errors
/// [`Error::I18n`] for a missing column name, [`Error::Csv`].
pub fn csv(report: &SalesReport, t: &Translator) -> Result<String, Error> {
    let mut out = Csv::new(
        t,
        &[
            "report-col-from",
            "report-col-to",
            "report-col-vat-rate",
            "report-col-gross",
            "report-col-vat",
            "report-col-net",
        ],
    )?;
    let total_label = t.t("report-total", None)?;
    let rows = report
        .rows
        .iter()
        .map(|r| {
            (
                csv_out::date(r.period.from()),
                csv_out::date(r.period.to()),
                r.totals,
            )
        })
        .chain(std::iter::once((total_label, String::new(), report.total)));
    for (from, to, totals) in rows {
        for rate in VatRate::ALL {
            let sum = totals.rate(rate);
            out.row(&[
                from.clone(),
                to.clone(),
                csv_out::rate(rate),
                csv_out::amount(sum.gross()),
                csv_out::amount(sum.vat()),
                csv_out::amount(sum.net()),
            ])?;
        }
    }
    out.finish()
}

/// Average paid against suggested price per pay-what-you-want product.
///
/// # Errors
/// [`Error::I18n`] for a missing column name, [`Error::Csv`].
pub fn pay_what_you_want_csv(report: &SalesReport, t: &Translator) -> Result<String, Error> {
    let mut out = Csv::new(
        t,
        &[
            "report-col-product",
            "report-col-pieces",
            "report-col-average-paid",
            "report-col-average-suggested",
        ],
    )?;
    for row in &report.pay_what_you_want {
        out.row(&[
            row.product_name.clone(),
            row.pieces.to_string(),
            csv_out::amount(row.average_paid()),
            csv_out::amount(row.average_suggested()),
        ])?;
    }
    out.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::FactLine;
    use domain::{BakeDayId, Line, OrderId};
    use jiff::Timestamp;
    use jiff::civil::date;
    use proptest::prelude::*;

    fn fact(id: i64, sold_at: &str, lines: Vec<FactLine>, refunded: i64) -> OrderFact {
        OrderFact {
            id: OrderId::new(id),
            number: OrderNumber {
                year: 2026,
                seq: i32::try_from(id).unwrap(),
            },
            bake_day_id: BakeDayId::new(1),
            drop_date: date(2026, 10, 31),
            drop_charity: None,
            sold_at: sold_at.parse::<Timestamp>().unwrap(),
            lines,
            refunded: Money::from_cents(refunded),
            fees: Money::ZERO,
            fee_missing: false,
        }
    }

    fn line(price: i64, qty: u32, rate: VatRate, suggested: Option<i64>) -> FactLine {
        FactLine {
            product_id: ProductId::new(if suggested.is_some() { 2 } else { 1 }),
            product_name: if suggested.is_some() {
                "Donatiebrood"
            } else {
                "Desem"
            }
            .into(),
            qty,
            unit_price: Money::from_cents(price),
            suggested_price: suggested.map(Money::from_cents),
            vat_rate: rate,
            estimated_cost: None,
        }
    }

    fn year() -> Period {
        Period::new(date(2026, 1, 1), date(2026, 12, 31)).unwrap()
    }

    #[test]
    fn buckets_use_amsterdam_dates_across_the_clock_change() {
        let orders = [
            // 00:30 on 1 October in Amsterdam (summer time): Q4.
            fact(
                1,
                "2026-09-30T22:30:00Z",
                vec![line(450, 1, VatRate::Nine, None)],
                0,
            ),
            // 23:30 on 30 September in Amsterdam: Q3.
            fact(
                2,
                "2026-09-30T21:30:00Z",
                vec![line(450, 1, VatRate::Nine, None)],
                0,
            ),
            // 00:30 on 26 October (winter time): still October.
            fact(
                3,
                "2026-10-25T23:30:00Z",
                vec![line(395, 1, VatRate::TwentyOne, None)],
                0,
            ),
        ];
        let report = build(&orders, year(), Granularity::Quarter).unwrap();
        assert_eq!(report.rows.len(), 2);
        assert_eq!(report.rows[0].period.from(), date(2026, 7, 1));
        assert_eq!(
            report.rows[0].totals.rate(VatRate::Nine).gross().cents(),
            450
        );
        assert_eq!(report.rows[1].period.from(), date(2026, 10, 1));
        assert_eq!(report.rows[1].totals.gross().unwrap().cents(), 845);

        let days = build(&orders, year(), Granularity::Day).unwrap();
        let froms: Vec<_> = days.rows.iter().map(|r| r.period.from()).collect();
        assert_eq!(
            froms,
            [date(2026, 9, 30), date(2026, 10, 1), date(2026, 10, 26)]
        );
    }

    #[test]
    fn orders_outside_the_period_are_left_out() {
        let october = Period::new(date(2026, 10, 1), date(2026, 10, 31)).unwrap();
        let orders = [fact(
            2,
            "2026-09-30T21:30:00Z",
            vec![line(450, 1, VatRate::Nine, None)],
            0,
        )];
        let report = build(&orders, october, Granularity::Month).unwrap();
        assert!(report.rows.is_empty());
        assert_eq!(report.total, VatTotals::default());
    }

    #[test]
    fn refunds_reduce_sales_and_mixed_partial_ones_are_flagged() {
        let orders = [
            fact(
                1,
                "2026-10-10T10:00:00Z",
                vec![line(450, 2, VatRate::Nine, None)],
                900,
            ),
            fact(
                2,
                "2026-10-10T10:00:00Z",
                vec![
                    line(450, 2, VatRate::Nine, None),
                    line(300, 1, VatRate::TwentyOne, None),
                ],
                100,
            ),
        ];
        let report = build(&orders, year(), Granularity::Month).unwrap();
        assert_eq!(report.total.rate(VatRate::Nine).gross().cents(), 825);
        assert_eq!(report.total.rate(VatRate::TwentyOne).gross().cents(), 275);
        assert_eq!(
            report.estimated_refunds,
            vec![OrderNumber { year: 2026, seq: 2 }]
        );
    }

    #[test]
    fn pay_what_you_want_averages() {
        let orders = [
            fact(
                1,
                "2026-10-10T10:00:00Z",
                vec![line(800, 2, VatRate::Nine, Some(600))],
                0,
            ),
            fact(
                2,
                "2026-10-11T10:00:00Z",
                vec![line(301, 1, VatRate::Nine, Some(600))],
                0,
            ),
            fact(
                3,
                "2026-10-11T10:00:00Z",
                vec![line(450, 1, VatRate::Nine, None)],
                0,
            ),
        ];
        let report = build(&orders, year(), Granularity::Month).unwrap();
        let row = &report.pay_what_you_want[0];
        assert_eq!(report.pay_what_you_want.len(), 1);
        assert_eq!(row.pieces, 3);
        assert_eq!(row.paid.cents(), 1901);
        // 1901 / 3 = 633.67
        assert_eq!(row.average_paid().cents(), 634);
        assert_eq!(row.average_suggested().cents(), 600);
        assert_eq!(average(Money::from_cents(-5), 2).cents(), -3);
        assert_eq!(average(Money::from_cents(5), 0), Money::ZERO);
    }

    #[test]
    fn csv_has_dutch_headers_and_rows_per_rate() {
        let t = Translator::dutch().unwrap();
        let orders = [fact(
            1,
            "2026-10-10T10:00:00Z",
            vec![line(450, 2, VatRate::Nine, Some(400))],
            0,
        )];
        let report = build(&orders, year(), Granularity::Month).unwrap();
        let text = csv(&report, &t).unwrap();
        let lines: Vec<_> = text.trim_start_matches('\u{feff}').lines().collect();
        assert_eq!(
            lines,
            [
                "Van;Tot en met;BTW-tarief;Omzet incl. BTW;BTW;Omzet excl. BTW",
                "01-10-2026;31-10-2026;9%;9,00;0,74;8,26",
                "01-10-2026;31-10-2026;21%;0,00;0,00;0,00",
                "Totaal;;9%;9,00;0,74;8,26",
                "Totaal;;21%;0,00;0,00;0,00",
            ]
        );
        let pwyw = pay_what_you_want_csv(&report, &t).unwrap();
        assert!(pwyw.ends_with("Donatiebrood;2;4,50;4,00\r\n"));
    }

    fn arb_order() -> impl Strategy<Value = OrderFact> {
        (
            1_i64..1_000,
            0_i64..(365 * 24),
            prop::collection::vec(
                (0_i64..3_000, 1_u32..5, any::<bool>()).prop_map(|(p, q, nine)| {
                    let rate = if nine {
                        VatRate::Nine
                    } else {
                        VatRate::TwentyOne
                    };
                    line(p, q, rate, None)
                }),
                1..4,
            ),
            0_i64..=100,
        )
            .prop_map(|(id, hours, lines, pct)| {
                let gross: i64 = lines
                    .iter()
                    .map(|l| l.line().total().unwrap().cents())
                    .sum();
                let at = "2026-01-01T00:00:00Z".parse::<Timestamp>().unwrap()
                    + jiff::SignedDuration::from_hours(hours);
                let mut f = fact(id, "2026-01-01T00:00:00Z", lines, gross * pct / 100);
                f.sold_at = at;
                f
            })
    }

    proptest! {
        #[test]
        fn btw_per_rate_sums_equal_the_line_sums(
            orders in prop::collection::vec(arb_order(), 0..12),
            granularity in prop_oneof![
                Just(Granularity::Day), Just(Granularity::Month), Just(Granularity::Quarter)
            ],
        ) {
            let report = build(&orders, year(), granularity).unwrap();
            let mut all: Vec<Line> = Vec::new();
            for order in &orders {
                if year().contains(order.sold_on()) {
                    all.extend(order.net_lines().unwrap().0);
                }
            }
            let from_lines = VatTotals::from_lines(&all).unwrap();
            prop_assert_eq!(report.total, from_lines);
            let mut from_rows = VatTotals::default();
            for row in &report.rows {
                from_rows = from_rows.checked_add(&row.totals).unwrap();
            }
            prop_assert_eq!(from_rows, from_lines);
            for rate in VatRate::ALL {
                let sum = from_lines.rate(rate);
                prop_assert_eq!(sum.net().cents() + sum.vat().cents(), sum.gross().cents());
            }
        }
    }
}
