# Vivado's clocking wizard's counters for requests, one per line of
# reqs.txt (wizard-requests.txt: input MHz, outputs MHz[@phase deg][%duty]),
# for PLLE2_ADV and MMCME2_ADV:
#
#   cp bench/cmt/wizard-requests.txt reqs.txt
#   /pool/dev/xc7-re/tools/vivado.sh -mode batch -source bench/cmt/wizard.tcl | grep ^RES
#
# Prints RES lines, as crates/chipsmith-xc7/tests/data/clk_wiz.txt holds them.
create_project -in_memory -part xc7z010clg400-1
set_property target_language Verilog [current_project]
proc gp {p ip} { if {[catch {get_property $p $ip} v]} { return "?" } ; return $v }
set fh [open reqs.txt r]
set reqs [split [string trim [read $fh]] "\n"]
close $fh
foreach prim {PLL MMCM} {
  set n wiz_$prim
  create_ip -name clk_wiz -vendor xilinx.com -library ip -module_name $n -dir .
  set_property CONFIG.PRIMITIVE $prim [get_ips $n]
  set nmax [expr {$prim eq "PLL" ? 6 : 7}]
  foreach r $reqs {
    set fin [lindex $r 0]
    set outs [split [lindex $r 1] ,]
    set d [list CONFIG.PRIM_IN_FREQ $fin CONFIG.NUM_OUT_CLKS [llength $outs]]
    for {set k 1} {$k <= $nmax} {incr k} {
      if {$k <= [llength $outs]} {
        set o [lindex $outs [expr {$k-1}]]
        set ph 0.000; set du 50.000
        regexp {%([-0-9.]+)} $o -> du
        regexp {@([-0-9.]+)} $o -> ph
        regsub {[@%].*} $o {} f
        if {$k > 1} { lappend d CONFIG.CLKOUT${k}_USED true }
        lappend d CONFIG.CLKOUT${k}_REQUESTED_OUT_FREQ $f CONFIG.CLKOUT${k}_REQUESTED_PHASE $ph CONFIG.CLKOUT${k}_REQUESTED_DUTY_CYCLE $du
      } elseif {$k > 1} { lappend d CONFIG.CLKOUT${k}_USED false }
    }
    if {[catch {set_property -dict $d [get_ips $n]} err]} { puts "RES $prim {$r} ERR [string map {"\n" " "} $err]"; continue }
    set ip [get_ips $n]
    set line "RES $prim {$r} M=[get_property CONFIG.MMCM_CLKFBOUT_MULT_F $ip] D=[get_property CONFIG.MMCM_DIVCLK_DIVIDE $ip] FBPH=[get_property CONFIG.MMCM_CLKFBOUT_PHASE $ip]"
    for {set k 0} {$k < [llength $outs]} {incr k} {
      set kk [expr {$k+1}]
      set dv [expr {$k == 0 && $prim eq "MMCM" ? "MMCM_CLKOUT0_DIVIDE_F" : "MMCM_CLKOUT${k}_DIVIDE"}]
      if {$k == 0} { set dv MMCM_CLKOUT0_DIVIDE_F }
      append line " O$k=[get_property CONFIG.$dv $ip]/[get_property CONFIG.MMCM_CLKOUT${k}_PHASE $ip]/[get_property CONFIG.MMCM_CLKOUT${k}_DUTY_CYCLE $ip]"
    }
    append line " BW=[get_property CONFIG.MMCM_BANDWIDTH $ip] COMP=[get_property CONFIG.MMCM_COMPENSATION $ip] REFJ=[gp CONFIG.MMCM_REF_JITTER1 $ip] CLKIN_P=[gp CONFIG.MMCM_CLKIN1_PERIOD $ip]"
    puts $line
  }
  # The generated instantiation, for the last request: what the wizard actually writes.
  generate_target synthesis [get_ips $n]
}
