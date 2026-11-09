`ifdef COCOTB_SIM
    `define COCOTB_CODE(name) \
        string __top_module; \
        string __vcd_file; \
        initial begin \
            if ($value$plusargs("TOP_MODULE=%s", __top_module) && __top_module == `"name`" && $value$plusargs("VCD_FILENAME=%s", __vcd_file)) begin \
                $dumpfile (__vcd_file); \
                $dumpvars (0, \name ); \
            end \
        end
`else
    `define COCOTB_CODE(name)
`endif


module \std::cdc::sync2[1884]  (
        input clk_i,
        input input_i,
        output output__
    );
    `COCOTB_CODE( std::cdc::sync2[1884] )
    logic \clk ;
    assign \clk  = clk_i;
    logic \input ;
    assign \input  = input_i;
    (* src = "<compiler dir>/stdlib/cdc.spade:14,14" *)
    reg \sync1 ;
    (* src = "<compiler dir>/stdlib/cdc.spade:15,14" *)
    reg \sync2 ;
    always @(posedge \clk ) begin
        \sync1  <= \input ;
    end
    always @(posedge \clk ) begin
        \sync2  <= \sync1 ;
    end
    assign output__ = \sync2 ;
endmodule


module \std::cdc::sync2_bool  (
        input clk_i,
        input input_i,
        output output__
    );
    `COCOTB_CODE( std::cdc::sync2_bool )
    logic \clk ;
    assign \clk  = clk_i;
    logic \input ;
    assign \input  = input_i;
    (* src = "<compiler dir>/stdlib/cdc.spade:31,14" *)
    logic _e_496;
    (* src = "<compiler dir>/stdlib/cdc.spade:31,14" *)
    
    \std::cdc::sync2[1884]  \sync2_0 (.clk_i(\clk ), .input_i(\input ), .output__(_e_496));
    assign output__ = _e_496;
endmodule


module \std::conv::tri_to_bool  (
        input t_i,
        output output__
    );
    `COCOTB_CODE( std::conv::tri_to_bool )
    logic \t ;
    assign \t  = t_i;
    (* src = "<compiler dir>/stdlib/conv.spade:80,5" *)
    logic _e_786;
    (* src = "<compiler dir>/stdlib/conv.spade:80,5" *)
    
    \std::conv::impl#13::as_bool  \as_bool_0 (.self_i(\t ), .output__(_e_786));
    assign output__ = _e_786;
endmodule


module \std::io::rising_edge  (
        input clk_i,
        input value_i,
        output output__
    );
    `COCOTB_CODE( std::io::rising_edge )
    logic \clk ;
    assign \clk  = clk_i;
    logic \value ;
    assign \value  = value_i;
    (* src = "<compiler dir>/stdlib/io.spade:12,14" *)
    reg \old_value ;
    (* src = "<compiler dir>/core/ops.spade:301,29" *)
    logic _e_2792;
    (* src = "<compiler dir>/core/ops.spade:301,9" *)
    logic _e_2791;
    (* src = "<compiler dir>/core/ops.spade:301,9" *)
    logic _e_1028;
    (* src = "<compiler dir>/core/ops.spade:343,29" *)
    logic _e_2800;
    (* src = "<compiler dir>/core/ops.spade:343,37" *)
    logic _e_2801;
    (* src = "<compiler dir>/core/ops.spade:343,9" *)
    logic _e_2799;
    (* src = "<compiler dir>/core/ops.spade:343,9" *)
    logic _e_1026;
    always @(posedge \clk ) begin
        \old_value  <= \value ;
    end
    assign _e_2792 = {\old_value };
    assign _e_2791 = ~_e_2792;
    assign _e_1028 = _e_2791;
    assign _e_2800 = {\value };
    assign _e_2801 = {_e_1028};
    assign _e_2799 = _e_2800 & _e_2801;
    assign _e_1026 = _e_2799;
    assign output__ = _e_1026;
endmodule


module \std::io::falling_edge  (
        input clk_i,
        input value_i,
        output output__
    );
    `COCOTB_CODE( std::io::falling_edge )
    logic \clk ;
    assign \clk  = clk_i;
    logic \value ;
    assign \value  = value_i;
    (* src = "<compiler dir>/stdlib/io.spade:27,14" *)
    reg \old_value ;
    (* src = "<compiler dir>/core/ops.spade:301,29" *)
    logic _e_2810;
    (* src = "<compiler dir>/core/ops.spade:301,9" *)
    logic _e_2809;
    (* src = "<compiler dir>/core/ops.spade:301,9" *)
    logic _e_1035;
    (* src = "<compiler dir>/core/ops.spade:343,29" *)
    logic _e_2818;
    (* src = "<compiler dir>/core/ops.spade:343,37" *)
    logic _e_2819;
    (* src = "<compiler dir>/core/ops.spade:343,9" *)
    logic _e_2817;
    (* src = "<compiler dir>/core/ops.spade:343,9" *)
    logic _e_1034;
    always @(posedge \clk ) begin
        \old_value  <= \value ;
    end
    assign _e_2810 = {\value };
    assign _e_2809 = ~_e_2810;
    assign _e_1035 = _e_2809;
    assign _e_2818 = {_e_1035};
    assign _e_2819 = {\old_value };
    assign _e_2817 = _e_2818 & _e_2819;
    assign _e_1034 = _e_2817;
    assign output__ = _e_1034;
endmodule


module top (
        input CLOCK_50,
        output[3:0] LEDR
    );
    `COCOTB_CODE( top )
    logic[3:0] \LEDR_mut ;
    assign LEDR = \LEDR_mut ;
    localparam[31:0] _e_1822 = 32'd0;
    localparam[31:0] _e_1826 = 32'd1;
    (* src = "src/main.spade:28,54" *)
    logic[32:0] _e_1824;
    (* src = "src/main.spade:28,48" *)
    logic[31:0] _e_1823;
    (* src = "src/main.spade:28,19" *)
    reg[31:0] \count ;
    localparam[31:0] _e_1829 = 32'd0;
    (* src = "src/main.spade:30,47" *)
    logic[31:0] _e_1832;
    (* src = "src/main.spade:30,47" *)
    logic[31:0] _e_1831;
    (* src = "src/main.spade:30,47" *)
    logic[31:0] _e_1830;
    (* src = "src/main.spade:30,19" *)
    reg[31:0] \gray ;
    localparam[31:0] _e_1837 = 32'd23;
    (* src = "src/main.spade:31,30" *)
    logic[31:0] _e_1835;
    (* src = "src/main.spade:31,24" *)
    logic[3:0] \top ;
    (* src = "<compiler dir>/stdlib/conv.spade:512,9" *)
    logic[3:0] _e_1840;
    (* src = "src/main.spade:34,26" *)
    logic _e_2744;
    (* src = "src/main.spade:34,26" *)
    logic _e_2745_mut;
    (* src = "src/main.spade:34,26" *)
    logic _e_1843;
    (* src = "src/main.spade:34,26" *)
    logic _e_1843_mut;
    (* src = "src/main.spade:34,9" *)
    logic \tms ;
    (* src = "src/main.spade:34,9" *)
    logic \tms_inv_mut ;
    (* src = "src/main.spade:35,26" *)
    logic _e_2746;
    (* src = "src/main.spade:35,26" *)
    logic _e_2747_mut;
    (* src = "src/main.spade:35,26" *)
    logic _e_1847;
    (* src = "src/main.spade:35,26" *)
    logic _e_1847_mut;
    (* src = "src/main.spade:35,9" *)
    logic \tck ;
    (* src = "src/main.spade:35,9" *)
    logic \tck_inv_mut ;
    (* src = "src/main.spade:36,26" *)
    logic _e_2748;
    (* src = "src/main.spade:36,26" *)
    logic _e_2749_mut;
    (* src = "src/main.spade:36,26" *)
    logic _e_1851;
    (* src = "src/main.spade:36,26" *)
    logic _e_1851_mut;
    (* src = "src/main.spade:36,9" *)
    logic \tdi ;
    (* src = "src/main.spade:36,9" *)
    logic \tdi_inv_mut ;
    (* src = "src/main.spade:37,30" *)
    logic _e_2750;
    (* src = "src/main.spade:37,30" *)
    logic _e_2751_mut;
    (* src = "src/main.spade:37,30" *)
    logic _e_1855;
    (* src = "src/main.spade:37,30" *)
    logic _e_1855_mut;
    (* src = "src/main.spade:37,9" *)
    logic \shift ;
    (* src = "src/main.spade:37,9" *)
    logic \shift_inv_mut ;
    (* src = "src/main.spade:38,30" *)
    logic _e_2752;
    (* src = "src/main.spade:38,30" *)
    logic _e_2753_mut;
    (* src = "src/main.spade:38,30" *)
    logic _e_1859;
    (* src = "src/main.spade:38,30" *)
    logic _e_1859_mut;
    (* src = "src/main.spade:38,9" *)
    logic \clkdr ;
    (* src = "src/main.spade:38,9" *)
    logic \clkdr_inv_mut ;
    (* src = "src/main.spade:39,32" *)
    logic _e_2754;
    (* src = "src/main.spade:39,32" *)
    logic _e_2755_mut;
    (* src = "src/main.spade:39,32" *)
    logic _e_1863;
    (* src = "src/main.spade:39,32" *)
    logic _e_1863_mut;
    (* src = "src/main.spade:39,9" *)
    logic \update ;
    (* src = "src/main.spade:39,9" *)
    logic \update_inv_mut ;
    (* src = "src/main.spade:40,34" *)
    logic _e_2756;
    (* src = "src/main.spade:40,34" *)
    logic _e_2757_mut;
    (* src = "src/main.spade:40,34" *)
    logic _e_1867;
    (* src = "src/main.spade:40,34" *)
    logic _e_1867_mut;
    (* src = "src/main.spade:40,9" *)
    logic \runidle ;
    (* src = "src/main.spade:40,9" *)
    logic \runidle_inv_mut ;
    (* src = "src/main.spade:41,28" *)
    logic _e_2758;
    (* src = "src/main.spade:41,28" *)
    logic _e_2759_mut;
    (* src = "src/main.spade:41,28" *)
    logic _e_1871;
    (* src = "src/main.spade:41,28" *)
    logic _e_1871_mut;
    (* src = "src/main.spade:41,9" *)
    logic \usr1 ;
    (* src = "src/main.spade:41,9" *)
    logic \usr1_inv_mut ;
    (* src = "src/main.spade:42,13" *)
    logic[3:0] \_ ;
    (* src = "src/main.spade:45,24" *)
    logic \tdo ;
    localparam[0:0] _e_1887 = 1;
    (* src = "<compiler dir>/core/ops.spade:53,9" *)
    logic _e_1885;
    (* src = "src/main.spade:58,28" *)
    logic \tck_clk ;
    localparam[63:0] _e_1903 = 64'd0;
    (* src = "<compiler dir>/core/ops.spade:343,29" *)
    logic _e_2834;
    (* src = "<compiler dir>/core/ops.spade:343,37" *)
    logic _e_2835;
    (* src = "<compiler dir>/core/ops.spade:343,9" *)
    logic _e_2833;
    (* src = "<compiler dir>/core/ops.spade:343,9" *)
    logic _e_1905;
    localparam[63:0] _e_1912 = 64'd1;
    (* src = "src/main.spade:60,36" *)
    logic[63:0] _e_1910;
    (* src = "src/main.spade:60,30" *)
    logic[62:0] \rest ;
    localparam[0:0] _e_1918 = 1;
    localparam[0:0] _e_1920 = 0;
    (* src = "src/main.spade:61,9" *)
    logic _e_1915;
    (* src = "src/main.spade:61,9" *)
    logic[63:0] _e_1914;
    localparam[31:0] _e_1924 = 32'd1397769249;
    (* src = "src/main.spade:64,9" *)
    logic[63:0] _e_1923;
    (* src = "src/main.spade:59,44" *)
    logic[63:0] _e_1904;
    (* src = "src/main.spade:59,18" *)
    reg[63:0] \dr ;
    assign _e_1824 = \count  + _e_1826;
    assign _e_1823 = _e_1824[31:0];
    initial begin
        \count  = 'b00000000000000000000000000000000;
    end
    always @(posedge \CLOCK_50 ) begin
        \count  <= _e_1823;
    end
    (* src = "src/main.spade:30,47" *)
    
    \std::num::impl#33::to_gray[1885]  \to_gray_0 (.self_i(\count ), .output__(_e_1832));
    (* src = "src/main.spade:30,47" *)
    
    \std::num::impl#38::as_bits[1886]  \as_bits_0 (.self_i(_e_1832), .output__(_e_1831));
    (* src = "src/main.spade:30,47" *)
    
    \std::conv::impl#10::as_uint[1887]  \as_uint_0 (.self_i(_e_1831), .output__(_e_1830));
    initial begin
        \gray  = 'b00000000000000000000000000000000;
    end
    always @(posedge \CLOCK_50 ) begin
        \gray  <= _e_1830;
    end
    assign _e_1835 = \count  >> _e_1837;
    assign \top  = _e_1835[3:0];
    (* src = "<compiler dir>/stdlib/conv.spade:512,9" *)
    
    \std::conv::impl#15::as_bits[1893]  \as_bits_1 (.self_i(\top ), .output__(_e_1840));
    assign \LEDR_mut  = _e_1840;
    
    
    assign _e_2744 = _e_2745_mut;
    assign _e_1843 = {_e_2744};
    assign {_e_2745_mut} = _e_1843_mut;
    assign \tms  = _e_1843;
    assign _e_1843_mut = \tms_inv_mut ;
    
    assign _e_2746 = _e_2747_mut;
    assign _e_1847 = {_e_2746};
    assign {_e_2747_mut} = _e_1847_mut;
    assign \tck  = _e_1847;
    assign _e_1847_mut = \tck_inv_mut ;
    
    assign _e_2748 = _e_2749_mut;
    assign _e_1851 = {_e_2748};
    assign {_e_2749_mut} = _e_1851_mut;
    assign \tdi  = _e_1851;
    assign _e_1851_mut = \tdi_inv_mut ;
    
    assign _e_2750 = _e_2751_mut;
    assign _e_1855 = {_e_2750};
    assign {_e_2751_mut} = _e_1855_mut;
    assign \shift  = _e_1855;
    assign _e_1855_mut = \shift_inv_mut ;
    
    assign _e_2752 = _e_2753_mut;
    assign _e_1859 = {_e_2752};
    assign {_e_2753_mut} = _e_1859_mut;
    assign \clkdr  = _e_1859;
    assign _e_1859_mut = \clkdr_inv_mut ;
    
    assign _e_2754 = _e_2755_mut;
    assign _e_1863 = {_e_2754};
    assign {_e_2755_mut} = _e_1863_mut;
    assign \update  = _e_1863;
    assign _e_1863_mut = \update_inv_mut ;
    
    assign _e_2756 = _e_2757_mut;
    assign _e_1867 = {_e_2756};
    assign {_e_2757_mut} = _e_1867_mut;
    assign \runidle  = _e_1867;
    assign _e_1867_mut = \runidle_inv_mut ;
    
    assign _e_2758 = _e_2759_mut;
    assign _e_1871 = {_e_2758};
    assign {_e_2759_mut} = _e_1871_mut;
    assign \usr1  = _e_1871;
    assign _e_1871_mut = \usr1_inv_mut ;
    assign \_  = {\tms , \clkdr , \update , \runidle };
    assign \tdo  = \dr [0:0];
    assign _e_1885 = \tdo  == _e_1887;
    (* src = "src/main.spade:46,13" *)
    
    CY2_JTAG \CY2_JTAG_0 (.TDOUSER(_e_1885), .TMSUTAP(\tms_inv_mut ), .TCKUTAP(\tck_inv_mut ), .TDIUTAP(\tdi_inv_mut ), .SHIFTUSER(\shift_inv_mut ), .CLKDRUSER(\clkdr_inv_mut ), .UPDATEUSER(\update_inv_mut ), .RUNIDLEUSER(\runidle_inv_mut ), .USR1USER(\usr1_inv_mut ));
    (* src = "src/main.spade:58,28" *)
    
    \std::conv::impl#11::as_clock  \as_clock_0 (.self_i(\tck ), .output__(\tck_clk ));
    assign _e_2834 = {\shift };
    assign _e_2835 = {\usr1 };
    assign _e_2833 = _e_2834 & _e_2835;
    assign _e_1905 = _e_2833;
    assign _e_1910 = \dr  >> _e_1912;
    assign \rest  = _e_1910[62:0];
    assign _e_1915 = \tdi  ? _e_1918 : _e_1920;
    (* src = "src/main.spade:61,9" *)
    
    \std::num::impl#33::concat[1895]  \concat_0 (.self_i(_e_1915), .other_i(\rest ), .output__(_e_1914));
    (* src = "src/main.spade:64,9" *)
    
    \std::num::impl#33::concat[1896]  \concat_1 (.self_i(_e_1924), .other_i(\gray ), .output__(_e_1923));
    assign _e_1904 = _e_1905 ? _e_1914 : _e_1923;
    initial begin
        \dr  = 'b0000000000000000000000000000000000000000000000000000000000000000;
    end
    always @(posedge \tck_clk ) begin
        \dr  <= _e_1904;
    end
    
endmodule


module \std::conv::impl#10::as_uint[1887]  (
        input[31:0] self_i,
        output[31:0] output__
    );
    `COCOTB_CODE( std::conv::impl#10::as_uint[1887] )
    logic[31:0] \self ;
    assign \self  = self_i;
    logic[31:0] _e_820;
    assign _e_820 = \self ;
    assign output__ = _e_820;
endmodule


module \std::conv::impl#11::as_clock  (
        input self_i,
        output output__
    );
    `COCOTB_CODE( std::conv::impl#11::as_clock )
    logic \self ;
    assign \self  = self_i;
    logic _e_830;
    assign _e_830 = \self ;
    assign output__ = _e_830;
endmodule


module \std::conv::impl#11::as_uint  (
        input self_i,
        output output__
    );
    `COCOTB_CODE( std::conv::impl#11::as_uint )
    logic \self ;
    assign \self  = self_i;
    logic _e_834;
    assign _e_834 = \self ;
    assign output__ = _e_834;
endmodule


module \std::conv::impl#12::as_bool  (
        input self_i,
        output output__
    );
    `COCOTB_CODE( std::conv::impl#12::as_bool )
    logic \self ;
    assign \self  = self_i;
    logic _e_845;
    assign _e_845 = \self ;
    assign output__ = _e_845;
endmodule


module \std::conv::impl#13::as_bool  (
        input self_i,
        output output__
    );
    `COCOTB_CODE( std::conv::impl#13::as_bool )
    logic \self ;
    assign \self  = self_i;
    logic _e_853;
    assign _e_853 = \self ;
    assign output__ = _e_853;
endmodule


module \std::conv::impl#15::as_bits[1893]  (
        input[3:0] self_i,
        output[3:0] output__
    );
    `COCOTB_CODE( std::conv::impl#15::as_bits[1893] )
    logic[3:0] \self ;
    assign \self  = self_i;
    logic[3:0] _e_875;
    assign _e_875 = \self ;
    assign output__ = _e_875;
endmodule


module \std::conv::impl#15::as_bits[1894]  (
        input[31:0] self_i,
        output[31:0] output__
    );
    `COCOTB_CODE( std::conv::impl#15::as_bits[1894] )
    logic[31:0] \self ;
    assign \self  = self_i;
    logic[31:0] _e_875;
    assign _e_875 = \self ;
    assign output__ = _e_875;
endmodule


module \std::default::impl#18::default  (
        output output__
    );
    `COCOTB_CODE( std::default::impl#18::default )
    localparam[0:0] _e_929 = 0;
    assign output__ = _e_929;
endmodule


module \std::default::impl#19::default  (
        
    );
    `COCOTB_CODE( std::default::impl#19::default )
    
endmodule


module \std::num::impl#33::concat[1896]  (
        input[31:0] self_i,
        input[31:0] other_i,
        output[63:0] output__
    );
    `COCOTB_CODE( std::num::impl#33::concat[1896] )
    logic[31:0] \self ;
    assign \self  = self_i;
    logic[31:0] \other ;
    assign \other  = other_i;
    (* src = "<compiler dir>/stdlib/num.spade:182,9" *)
    logic[63:0] _e_1338;
    assign _e_1338 = {\self , \other };
    assign output__ = _e_1338;
endmodule


module \std::num::impl#33::concat[1895]  (
        input self_i,
        input[62:0] other_i,
        output[63:0] output__
    );
    `COCOTB_CODE( std::num::impl#33::concat[1895] )
    logic \self ;
    assign \self  = self_i;
    logic[62:0] \other ;
    assign \other  = other_i;
    (* src = "<compiler dir>/stdlib/num.spade:182,9" *)
    logic[63:0] _e_1338;
    assign _e_1338 = {\self , \other };
    assign output__ = _e_1338;
endmodule


module \std::num::impl#33::to_gray[1885]  (
        input[31:0] self_i,
        output[31:0] output__
    );
    `COCOTB_CODE( std::num::impl#33::to_gray[1885] )
    logic[31:0] \self ;
    assign \self  = self_i;
    localparam[31:0] _e_1374 = 32'd1;
    (* src = "<compiler dir>/stdlib/num.spade:251,14" *)
    logic[31:0] _e_1372;
    logic[31:0] \lhs ;
    logic[31:0] \rhs ;
    (* src = "<compiler dir>/core/ops.spade:463,34" *)
    logic[31:0] _e_2845;
    logic[31:0] _e_1371;
    (* src = "<compiler dir>/stdlib/num.spade:251,9" *)
    logic[31:0] _e_1370;
    (* src = "<compiler dir>/stdlib/num.spade:251,14" *)
    
    \core::ops::impl#99::wrapping_shr[1890]  \wrapping_shr_0 (.self_i(\self ), .rhs_i(_e_1374), .output__(_e_1372));
    assign \lhs  = _e_1372;
    assign \rhs  = \self ;
    assign _e_2845 = \lhs  ^ \rhs ;
    assign _e_1371 = _e_2845;
    assign _e_1370 = {_e_1371};
    assign output__ = _e_1370;
endmodule


module \std::num::impl#38::as_bits[1886]  (
        input[31:0] self_i,
        output[31:0] output__
    );
    `COCOTB_CODE( std::num::impl#38::as_bits[1886] )
    logic[31:0] \self ;
    assign \self  = self_i;
    (* src = "<compiler dir>/stdlib/num.spade:362,9" *)
    logic[31:0] _e_1401;
    (* src = "<compiler dir>/stdlib/num.spade:362,9" *)
    logic[31:0] _e_1400;
    assign _e_1401 = \self [31:0];
    (* src = "<compiler dir>/stdlib/num.spade:362,9" *)
    
    \std::conv::impl#15::as_bits[1894]  \as_bits_0 (.self_i(_e_1401), .output__(_e_1400));
    assign output__ = _e_1400;
endmodule


module \core::ops::impl#99::wrapping_shr[1890]  (
        input[31:0] self_i,
        input[31:0] rhs_i,
        output[31:0] output__
    );
    `COCOTB_CODE( core::ops::impl#99::wrapping_shr[1890] )
    logic[31:0] \self ;
    assign \self  = self_i;
    logic[31:0] \rhs ;
    assign \rhs  = rhs_i;
    (* src = "<compiler dir>/core/ops.spade:290,9" *)
    logic[31:0] _e_2345;
    assign _e_2345 = \self  >> \rhs ;
    assign output__ = _e_2345;
endmodule

`default_nettype none