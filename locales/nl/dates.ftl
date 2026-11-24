# $n: ISO weekday number, 1 = maandag … 7 = zondag
weekday = { $n ->
    [1] maandag
    [2] dinsdag
    [3] woensdag
    [4] donderdag
    [5] vrijdag
    [6] zaterdag
   *[7] zondag
}
weekday-short = { $n ->
    [1] ma
    [2] di
    [3] wo
    [4] do
    [5] vr
    [6] za
   *[7] zo
}
# $n: month number, 1 = januari … 12 = december
month = { $n ->
    [1] januari
    [2] februari
    [3] maart
    [4] april
    [5] mei
    [6] juni
    [7] juli
    [8] augustus
    [9] september
    [10] oktober
    [11] november
   *[12] december
}
month-short = { $n ->
    [1] jan
    [2] feb
    [3] mrt
    [4] apr
    [5] mei
    [6] jun
    [7] jul
    [8] aug
    [9] sep
    [10] okt
    [11] nov
   *[12] dec
}
# zaterdag 5 december 2026
date-long = { $weekday } { $day } { $month } { $year }
# za 5 dec
date-short = { $weekday } { $day } { $month }
