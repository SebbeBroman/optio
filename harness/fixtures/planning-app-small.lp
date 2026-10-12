\ File written by HiGHS .lp file handler
min
 obj: -0.1 y_g1_1 +0.05 peak_ts_1 +1 range_ts_1 +0.05 peak_ts_2 +1 range_ts_2 
st
 yone_g1: +1 y_g1_1 = +1
 roundlink_g1_ts_1_ring_a: +1 x_g1_ts_1_ring_a -1 xr_g1_quarterfinal_ts_1_ring_a = +0
 roundlink_g1_ts_2_ring_a: +1 x_g1_ts_2_ring_a -1 xr_g1_final_ts_2_ring_a -1 xr_g1_semifinal_ts_2_ring_a = +0
 tot_g1: +1 x_g1_ts_1_ring_a +1 x_g1_ts_2_ring_a = +7
 day_g1_0: -4 y_g1_1 +1 x_g1_ts_1_ring_a = +0
 roundday_g1_0_quarterfinal: -4 y_g1_1 +1 xr_g1_quarterfinal_ts_1_ring_a = +0
 day_g1_1: -3 y_g1_1 +1 x_g1_ts_2_ring_a = +0
 roundday_g1_1_final: -1 y_g1_1 +1 xr_g1_final_ts_2_ring_a = +0
 roundday_g1_1_semifinal: -2 y_g1_1 +1 xr_g1_semifinal_ts_2_ring_a = +0
 rangerel_ts_1: -1 peak_ts_1 +1 min_ts_1 +1 range_ts_1 >= +0
 peakge_ts_1_ring_a: +900 x_g1_ts_1_ring_a -1 peak_ts_1 <= -0
 minle_ts_1_ring_a: +900 x_g1_ts_1_ring_a -1 min_ts_1 >= -0
 rangerel_ts_2: -1 peak_ts_2 +1 min_ts_2 +1 range_ts_2 >= +0
 peakge_ts_2_ring_a: +900 x_g1_ts_2_ring_a -1 peak_ts_2 <= -0
 minle_ts_2_ring_a: +900 x_g1_ts_2_ring_a -1 min_ts_2 >= -0
bounds
 y_g1_1 <= 1
 x_g1_ts_1_ring_a <= 4
 xr_g1_quarterfinal_ts_1_ring_a <= 4
 x_g1_ts_2_ring_a <= 3
 xr_g1_final_ts_2_ring_a <= 1
 xr_g1_semifinal_ts_2_ring_a <= 2
 peak_ts_1 <= 1000000000
 min_ts_1 <= 1000000000
 range_ts_1 <= 1000000000
 peak_ts_2 <= 1000000000
 min_ts_2 <= 1000000000
 range_ts_2 <= 1000000000
bin
 y_g1_1
 xr_g1_final_ts_2_ring_a
gen
 x_g1_ts_1_ring_a
 xr_g1_quarterfinal_ts_1_ring_a
 x_g1_ts_2_ring_a
 xr_g1_semifinal_ts_2_ring_a
semi
end
