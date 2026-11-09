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


module \std::cdc::sync2[1915]  (
        input clk_i,
        input[3:0] input_i,
        output[3:0] output__
    );
    `COCOTB_CODE( std::cdc::sync2[1915] )
    logic \clk ;
    assign \clk  = clk_i;
    logic[3:0] \input ;
    assign \input  = input_i;
    (* src = "<compiler dir>/stdlib/cdc.spade:14,14" *)
    reg[3:0] \sync1 ;
    (* src = "<compiler dir>/stdlib/cdc.spade:15,14" *)
    reg[3:0] \sync2 ;
    always @(posedge \clk ) begin
        \sync1  <= \input ;
    end
    always @(posedge \clk ) begin
        \sync2  <= \sync1 ;
    end
    assign output__ = \sync2 ;
endmodule


module \std::cdc::sync2[1893]  (
        input clk_i,
        input input_i,
        output output__
    );
    `COCOTB_CODE( std::cdc::sync2[1893] )
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


module \std::cdc::sync_uint_counter[1911]  (
        input source_clk_i,
        input dest_clk_i,
        input[3:0] input_i,
        output[3:0] output__
    );
    `COCOTB_CODE( std::cdc::sync_uint_counter[1911] )
    logic \source_clk ;
    assign \source_clk  = source_clk_i;
    logic \dest_clk ;
    assign \dest_clk  = dest_clk_i;
    logic[3:0] \input ;
    assign \input  = input_i;
    (* src = "<compiler dir>/stdlib/cdc.spade:23,28" *)
    logic[3:0] _e_488;
    (* src = "<compiler dir>/stdlib/cdc.spade:23,21" *)
    reg[3:0] \gray ;
    (* src = "<compiler dir>/stdlib/cdc.spade:24,14" *)
    logic[3:0] _e_492;
    (* src = "<compiler dir>/stdlib/cdc.spade:24,5" *)
    logic[3:0] _e_490;
    (* src = "<compiler dir>/stdlib/cdc.spade:23,28" *)
    
    \std::num::impl#33::to_gray[1913]  \to_gray_0 (.self_i(\input ), .output__(_e_488));
    always @(posedge \source_clk ) begin
        \gray  <= _e_488;
    end
    (* src = "<compiler dir>/stdlib/cdc.spade:24,14" *)
    
    \std::cdc::sync2[1915]  \sync2_0 (.clk_i(\dest_clk ), .input_i(\gray ), .output__(_e_492));
    (* src = "<compiler dir>/stdlib/cdc.spade:24,5" *)
    
    \std::num::impl#38::to_uint[1918]  \to_uint_0 (.self_i(_e_492), .output__(_e_490));
    assign output__ = _e_490;
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
    logic _e_497;
    (* src = "<compiler dir>/stdlib/cdc.spade:31,14" *)
    
    \std::cdc::sync2[1893]  \sync2_0 (.clk_i(\clk ), .input_i(\input ), .output__(_e_497));
    assign output__ = _e_497;
endmodule


module \std::conv::tri_to_bool  (
        input t_i,
        output output__
    );
    `COCOTB_CODE( std::conv::tri_to_bool )
    logic \t ;
    assign \t  = t_i;
    (* src = "<compiler dir>/stdlib/conv.spade:80,5" *)
    logic _e_787;
    (* src = "<compiler dir>/stdlib/conv.spade:80,5" *)
    
    \std::conv::impl#13::as_bool  \as_bool_0 (.self_i(\t ), .output__(_e_787));
    assign output__ = _e_787;
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
    logic _e_3015;
    (* src = "<compiler dir>/core/ops.spade:301,9" *)
    logic _e_3014;
    (* src = "<compiler dir>/core/ops.spade:301,9" *)
    logic _e_1029;
    (* src = "<compiler dir>/core/ops.spade:343,29" *)
    logic _e_3023;
    (* src = "<compiler dir>/core/ops.spade:343,37" *)
    logic _e_3024;
    (* src = "<compiler dir>/core/ops.spade:343,9" *)
    logic _e_3022;
    (* src = "<compiler dir>/core/ops.spade:343,9" *)
    logic _e_1027;
    always @(posedge \clk ) begin
        \old_value  <= \value ;
    end
    assign _e_3015 = {\old_value };
    assign _e_3014 = ~_e_3015;
    assign _e_1029 = _e_3014;
    assign _e_3023 = {\value };
    assign _e_3024 = {_e_1029};
    assign _e_3022 = _e_3023 & _e_3024;
    assign _e_1027 = _e_3022;
    assign output__ = _e_1027;
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
    logic _e_3033;
    (* src = "<compiler dir>/core/ops.spade:301,9" *)
    logic _e_3032;
    (* src = "<compiler dir>/core/ops.spade:301,9" *)
    logic _e_1036;
    (* src = "<compiler dir>/core/ops.spade:343,29" *)
    logic _e_3041;
    (* src = "<compiler dir>/core/ops.spade:343,37" *)
    logic _e_3042;
    (* src = "<compiler dir>/core/ops.spade:343,9" *)
    logic _e_3040;
    (* src = "<compiler dir>/core/ops.spade:343,9" *)
    logic _e_1035;
    always @(posedge \clk ) begin
        \old_value  <= \value ;
    end
    assign _e_3033 = {\value };
    assign _e_3032 = ~_e_3033;
    assign _e_1036 = _e_3032;
    assign _e_3041 = {_e_1036};
    assign _e_3042 = {\old_value };
    assign _e_3040 = _e_3041 & _e_3042;
    assign _e_1035 = _e_3040;
    assign output__ = _e_1035;
endmodule


module \std::mem::read_read_port[1917]  (
        input read_clk_i,
        input[3:0] addr_i,
        input[7:0] p_i, output[3:0] p_o,
        output[7:0] output__
    );
    `COCOTB_CODE( std::mem::read_read_port[1917] )
    logic \read_clk ;
    assign \read_clk  = read_clk_i;
    logic[3:0] \addr ;
    assign \addr  = addr_i;
    logic[7:0] \p ;
    assign \p  = p_i;
    logic[3:0] \p_mut ;
    assign p_o = \p_mut ;
    (* src = "<compiler dir>/stdlib/mem.spade:70,1" *)
    reg[3:0] \s1_addr ;
    (* src = "<compiler dir>/stdlib/mem.spade:69,18" *)
    logic[7:0] \result ;
    always @(posedge \read_clk ) begin
        \s1_addr  <= \addr ;
    end
    (* src = "<compiler dir>/stdlib/mem.spade:69,18" *)
    
    \std::mem::impl#31::read[1926]  \read_0 (.self_i(\p ), .self_o(\p_mut ), .read_clk_i(\read_clk ), .addr_i(\addr ), .output__(\result ));
    assign output__ = \result ;
endmodule


module \std::mem::dp_bram[1904]  (
        input write_clk_i,
        input read_clk_i,
        output[7:0] output__,
        input[16:0] input__
    );
    `COCOTB_CODE( std::mem::dp_bram[1904] )
    logic \write_clk ;
    assign \write_clk  = write_clk_i;
    logic \read_clk ;
    assign \read_clk  = read_clk_i;
    (* src = "<compiler dir>/stdlib/mem.spade:77,5" *)
    logic[7:0] _e_1076[0:15];
    (* src = "<compiler dir>/stdlib/mem.spade:78,9" *)
    logic[8:0] \read_port ;
    (* src = "<compiler dir>/stdlib/mem.spade:78,9" *)
    logic[5:0] \read_port_mut ;
    (* src = "<compiler dir>/stdlib/mem.spade:79,9" *)
    logic[13:0] \write_port_mut ;
    (* src = "<compiler dir>/stdlib/mem.spade:82,22" *)
    logic[3:0] _e_2819;
    (* src = "<compiler dir>/stdlib/mem.spade:82,22" *)
    logic[3:0] _e_2820_mut;
    (* src = "<compiler dir>/stdlib/mem.spade:82,22" *)
    logic[3:0] \write_addr ;
    (* src = "<compiler dir>/stdlib/mem.spade:82,22" *)
    logic[3:0] \write_addr_mut ;
    (* src = "<compiler dir>/stdlib/mem.spade:83,22" *)
    logic[8:0] _e_2821;
    (* src = "<compiler dir>/stdlib/mem.spade:83,22" *)
    logic[8:0] _e_2822_mut;
    (* src = "<compiler dir>/stdlib/mem.spade:83,22" *)
    logic[8:0] \write_data ;
    (* src = "<compiler dir>/stdlib/mem.spade:83,22" *)
    logic[8:0] \write_data_mut ;
    (* src = "<compiler dir>/stdlib/mem.spade:85,27" *)
    logic[8:0] _e_1082;
    (* src = "<compiler dir>/stdlib/mem.spade:85,21" *)
    logic[8:0] _e_1084;
    logic _e_2827;
    (* src = "<compiler dir>/stdlib/mem.spade:86,17" *)
    logic[12:0] _e_1085;
    (* src = "<compiler dir>/stdlib/mem.spade:85,21" *)
    logic[8:0] _e_1087;
    (* src = "<compiler dir>/stdlib/mem.spade:87,9" *)
    logic[7:0] \data ;
    logic _e_2830;
    localparam[0:0] _e_2831 = 1;
    logic _e_2832;
    (* src = "<compiler dir>/stdlib/mem.spade:87,29" *)
    logic[3:0] _e_1090;
    (* src = "<compiler dir>/stdlib/mem.spade:87,28" *)
    logic[11:0] _e_1089;
    (* src = "<compiler dir>/stdlib/mem.spade:87,23" *)
    logic[12:0] _e_1088;
    (* src = "<compiler dir>/stdlib/mem.spade:85,21" *)
    logic[12:0] \write_req ;
    (* src = "<compiler dir>/stdlib/mem.spade:92,21" *)
    logic[3:0] _e_2855;
    (* src = "<compiler dir>/stdlib/mem.spade:92,21" *)
    logic[3:0] _e_2856_mut;
    (* src = "<compiler dir>/stdlib/mem.spade:92,21" *)
    logic[3:0] \read_addr ;
    (* src = "<compiler dir>/stdlib/mem.spade:92,21" *)
    logic[3:0] \read_addr_mut ;
    (* src = "<compiler dir>/stdlib/mem.spade:93,59" *)
    logic[3:0] _e_1104;
    (* src = "<compiler dir>/stdlib/mem.spade:93,54" *)
    logic[4:0] _e_1103;
    (* src = "<compiler dir>/stdlib/mem.spade:93,21" *)
    logic[8:0] \read_data ;
    (* src = "<compiler dir>/stdlib/mem.spade:97,19" *)
    logic[3:0] _e_1110_mut;
    (* src = "<compiler dir>/stdlib/mem.spade:98,20" *)
    logic[8:0] _e_1112_mut;
    (* src = "<compiler dir>/stdlib/mem.spade:96,9" *)
    logic[12:0] _e_1109_mut;
    (* src = "<compiler dir>/stdlib/mem.spade:101,19" *)
    logic[3:0] _e_1115_mut;
    (* src = "<compiler dir>/stdlib/mem.spade:102,18" *)
    logic[7:0] _e_1117;
    (* src = "<compiler dir>/stdlib/mem.spade:100,9" *)
    logic[7:0] _e_1114;
    (* src = "<compiler dir>/stdlib/mem.spade:100,9" *)
    logic[3:0] _e_1114_mut;
    (* src = "<compiler dir>/stdlib/mem.spade:95,5" *)
    logic[7:0] _e_1108;
    (* src = "<compiler dir>/stdlib/mem.spade:95,5" *)
    logic[16:0] _e_1108_mut;
    always @(posedge \read_port_mut [5]) begin
        if (\read_port_mut [4]) begin
            \read_port [8] <= 1'b1;
            \read_port [7:0] <= _e_1076[\read_port_mut [3:0]];
        end else begin
            \read_port [8] <= 1'b0;
            \read_port [7:0] <= 'sbx;
        end
    end
    always @(posedge \write_port_mut [13]) begin
        if (\write_port_mut [12]) begin
            _e_1076[\write_port_mut [11:8]] <= \write_port_mut [7:0];
        end
    end
    
    assign _e_2819 = _e_2820_mut;
    assign \write_addr  = {_e_2819};
    assign {_e_2820_mut} = \write_addr_mut ;
    
    assign _e_2821 = _e_2822_mut;
    assign \write_data  = {_e_2821};
    assign {_e_2822_mut} = \write_data_mut ;
    assign _e_1082 = \write_data [8:0];
    
    assign _e_2827 = _e_1084[8] == 1'd0;
    assign _e_1085 = {1'd0, 12'bX};
    
    assign \data  = _e_1087[7:0];
    assign _e_2830 = _e_1087[8] == 1'd1;
    assign _e_2832 = _e_2830 && _e_2831;
    assign _e_1090 = \write_addr [3:0];
    assign _e_1089 = {_e_1090, \data };
    assign _e_1088 = {1'd1, _e_1089};
    always_comb begin
        priority casez ({_e_2827, _e_2832})
            2'b1?: \write_req  = _e_1085;
            2'b01: \write_req  = _e_1088;
            2'b?: \write_req  = 13'dx;
        endcase
    end
    assign _e_1084 = _e_1082;
    assign _e_1087 = _e_1082;
    (* src = "<compiler dir>/stdlib/mem.spade:90,5" *)
    
    \core::mem::impl#73::write[1912]  \write_0 (.self_o(\write_port_mut ), .clk_i(\write_clk ), .req_i(\write_req ));
    
    assign _e_2855 = _e_2856_mut;
    assign \read_addr  = {_e_2855};
    assign {_e_2856_mut} = \read_addr_mut ;
    assign _e_1104 = \read_addr [3:0];
    assign _e_1103 = {1'd1, _e_1104};
    (* src = "<compiler dir>/stdlib/mem.spade:93,21" *)
    
    \core::mem::impl#71::read[1916]  \read_0 (.self_i(\read_port ), .self_o(\read_port_mut ), .clk_i(\read_clk ), .req_i(_e_1103), .output__(\read_data ));
    assign \write_addr_mut [3:0] = _e_1110_mut;
    assign \write_data_mut [8:0] = _e_1112_mut;
    assign {_e_1110_mut, _e_1112_mut} = _e_1109_mut;
    assign \read_addr_mut [3:0] = _e_1115_mut;
    (* src = "<compiler dir>/stdlib/mem.spade:102,18" *)
    
    \std::option::impl#41::unwrap_or_undef[1920]  \unwrap_or_undef_0 (.self_i(\read_data ), .output__(_e_1117));
    assign _e_1114 = {_e_1117};
    assign {_e_1115_mut} = _e_1114_mut;
    assign _e_1108 = {_e_1114};
    assign {_e_1109_mut, _e_1114_mut} = _e_1108_mut;
    assign output__ = _e_1108;
    assign _e_1108_mut = input__;
endmodule


module \std::mem::fifo_impl::fifo_read[1909]  (
        input read_clk_i,
        input read_rst_i,
        input[7:0] mem_i, output[3:0] mem_o,
        input[3:0] to_w_i, output[3:0] to_w_o,
        input write_clk_i,
        output[12:0] output__,
        input input__
    );
    `COCOTB_CODE( std::mem::fifo_impl::fifo_read[1909] )
    logic \read_clk ;
    assign \read_clk  = read_clk_i;
    logic \read_rst ;
    assign \read_rst  = read_rst_i;
    logic[7:0] \mem ;
    assign \mem  = mem_i;
    logic[3:0] \mem_mut ;
    assign mem_o = \mem_mut ;
    logic[3:0] \to_w ;
    assign \to_w  = to_w_i;
    logic[3:0] \to_w_mut ;
    assign to_w_o = \to_w_mut ;
    logic \write_clk ;
    assign \write_clk  = write_clk_i;
    (* src = "<compiler dir>/stdlib/mem.spade:151,24" *)
    logic[3:0] _e_1124;
    (* src = "<compiler dir>/stdlib/mem.spade:148,13" *)
    logic[3:0] \write_ptr ;
    localparam[0:0] write_clk_n1 = 0;
    (* src = "<compiler dir>/stdlib/mem.spade:156,19" *)
    logic _e_2843;
    (* src = "<compiler dir>/stdlib/mem.spade:156,19" *)
    logic _e_2844_mut;
    (* src = "<compiler dir>/stdlib/mem.spade:156,19" *)
    logic \ack ;
    (* src = "<compiler dir>/stdlib/mem.spade:156,19" *)
    logic \ack_mut ;
    (* src = "<compiler dir>/core/ops.spade:53,9" *)
    logic \empty ;
    (* src = "<compiler dir>/stdlib/mem.spade:160,36" *)
    logic _e_1137;
    (* src = "<compiler dir>/stdlib/mem.spade:160,35" *)
    logic[1:0] _e_1136;
    (* src = "<compiler dir>/stdlib/mem.spade:160,29" *)
    logic[1:0] _e_1142;
    (* src = "<compiler dir>/stdlib/mem.spade:161,13" *)
    logic _e_1140;
    (* src = "<compiler dir>/stdlib/mem.spade:161,13" *)
    logic _e_1141;
    logic _e_2849;
    (* src = "<compiler dir>/stdlib/mem.spade:160,29" *)
    logic[1:0] _e_1146;
    (* src = "<compiler dir>/stdlib/mem.spade:162,13" *)
    logic _e_1144;
    (* src = "<compiler dir>/stdlib/mem.spade:162,13" *)
    logic _e_1145;
    logic _e_2853;
    logic _e_2854;
    localparam[3:0] _e_1150 = 1;
    (* src = "<compiler dir>/stdlib/mem.spade:162,36" *)
    logic[4:0] _e_1148;
    (* src = "<compiler dir>/stdlib/mem.spade:162,30" *)
    logic[3:0] _e_1147;
    (* src = "<compiler dir>/stdlib/mem.spade:160,29" *)
    logic[1:0] _e_1153;
    (* src = "<compiler dir>/stdlib/mem.spade:163,13" *)
    logic _e_1151;
    (* src = "<compiler dir>/stdlib/mem.spade:163,13" *)
    logic \_ ;
    logic _e_2859;
    localparam[0:0] _e_2860 = 1;
    logic _e_2861;
    (* src = "<compiler dir>/stdlib/mem.spade:160,29" *)
    logic[3:0] \read_ptr_next ;
    localparam[3:0] _e_1159 = 0;
    (* src = "<compiler dir>/stdlib/mem.spade:165,23" *)
    reg[3:0] \read_ptr ;
    (* src = "<compiler dir>/stdlib/mem.spade:167,19" *)
    logic[7:0] \out ;
    (* src = "<compiler dir>/core/ops.spade:500,9" *)
    logic _e_1169;
    localparam[3:0] _e_1176 = 0;
    (* src = "<compiler dir>/stdlib/mem.spade:177,58" *)
    logic[4:0] _e_1174;
    localparam[4:0] _e_1177 = 16;
    (* src = "<compiler dir>/stdlib/mem.spade:177,58" *)
    logic[5:0] _e_1173;
    logic[5:0] _e_1179;
    (* src = "<compiler dir>/stdlib/mem.spade:177,33" *)
    logic[5:0] \write_ptr_abs ;
    logic[5:0] _e_1185;
    (* src = "<compiler dir>/stdlib/mem.spade:179,19" *)
    logic[6:0] _e_1183;
    (* src = "<compiler dir>/stdlib/mem.spade:179,13" *)
    logic[3:0] \num_elements ;
    (* src = "<compiler dir>/stdlib/mem.spade:182,13" *)
    logic[3:0] _e_1188_mut;
    (* src = "<compiler dir>/core/ops.spade:301,29" *)
    logic _e_3057;
    (* src = "<compiler dir>/core/ops.spade:301,9" *)
    logic _e_3056;
    (* src = "<compiler dir>/core/ops.spade:301,9" *)
    logic _e_1194;
    (* src = "<compiler dir>/stdlib/mem.spade:185,31" *)
    logic[8:0] _e_1197;
    (* src = "<compiler dir>/stdlib/mem.spade:185,50" *)
    logic[8:0] _e_1200;
    (* src = "<compiler dir>/stdlib/mem.spade:185,19" *)
    logic[8:0] _e_1193;
    (* src = "<compiler dir>/stdlib/mem.spade:186,18" *)
    logic _e_1201_mut;
    (* src = "<compiler dir>/stdlib/mem.spade:184,9" *)
    logic[12:0] _e_1192;
    (* src = "<compiler dir>/stdlib/mem.spade:184,9" *)
    logic _e_1192_mut;
    assign _e_1124 = \to_w [3:0];
    (* src = "<compiler dir>/stdlib/mem.spade:148,13" *)
    
    \std::cdc::sync_uint_counter[1911]  \sync_uint_counter_0 (.source_clk_i(\write_clk ), .dest_clk_i(\read_clk ), .input_i(_e_1124), .output__(\write_ptr ));
    
    assign _e_2843 = _e_2844_mut;
    assign \ack  = {_e_2843};
    assign {_e_2844_mut} = \ack_mut ;
    assign \empty  = \write_ptr  == \read_ptr ;
    assign _e_1137 = \ack ;
    assign _e_1136 = {_e_1137, \empty };
    
    assign _e_1140 = _e_1142[1];
    assign _e_1141 = _e_1142[0];
    assign _e_2849 = _e_1140 && _e_1141;
    
    assign _e_1144 = _e_1146[1];
    assign _e_1145 = _e_1146[0];
    assign _e_2853 = !_e_1145;
    assign _e_2854 = _e_1144 && _e_2853;
    assign _e_1148 = \read_ptr  + _e_1150;
    assign _e_1147 = _e_1148[3:0];
    
    assign _e_1151 = _e_1153[1];
    assign \_  = _e_1153[0];
    assign _e_2859 = !_e_1151;
    assign _e_2861 = _e_2859 && _e_2860;
    always_comb begin
        priority casez ({_e_2849, _e_2854, _e_2861})
            3'b1??: \read_ptr_next  = \read_ptr ;
            3'b01?: \read_ptr_next  = _e_1147;
            3'b001: \read_ptr_next  = \read_ptr ;
            3'b?: \read_ptr_next  = 4'dx;
        endcase
    end
    assign _e_1142 = _e_1136;
    assign _e_1146 = _e_1136;
    assign _e_1153 = _e_1136;
    always @(posedge \read_clk ) begin
        if (\read_rst ) begin
            \read_ptr  <= _e_1159;
        end
        else begin
            \read_ptr  <= \read_ptr_next ;
        end
    end
    (* src = "<compiler dir>/stdlib/mem.spade:167,19" *)
    
    \std::mem::read_read_port[1917]  \read_read_port_0 (.read_clk_i(\read_clk ), .addr_i(\read_ptr_next ), .p_i(\mem ), .p_o(\mem_mut ), .output__(\out ));
    assign _e_1169 = \write_ptr  < \read_ptr ;
    assign _e_1174 = \write_ptr  + _e_1176;
    assign _e_1173 = _e_1174 + _e_1177;
    assign _e_1179 = {2'b0, \write_ptr };
    assign \write_ptr_abs  = _e_1169 ? _e_1173 : _e_1179;
    assign _e_1185 = {2'b0, \read_ptr };
    assign _e_1183 = \write_ptr_abs  - _e_1185;
    assign \num_elements  = _e_1183[3:0];
    assign \to_w_mut [3:0] = _e_1188_mut;
    assign _e_1188_mut = \read_ptr ;
    
    assign _e_3057 = {\empty };
    assign _e_3056 = ~_e_3057;
    assign _e_1194 = _e_3056;
    assign _e_1197 = {1'd1, \out };
    assign _e_1200 = {1'd0, 8'bX};
    assign _e_1193 = _e_1194 ? _e_1197 : _e_1200;
    assign \ack_mut  = _e_1201_mut;
    assign _e_1192 = {_e_1193, \num_elements };
    assign {_e_1201_mut} = _e_1192_mut;
    assign output__ = _e_1192;
    assign _e_1192_mut = input__;
endmodule


module \std::mem::fifo_impl::fifo_write[1907]  (
        input write_clk_i,
        input write_rst_i,
        output[12:0] mem_o,
        input[3:0] from_w_i, output[3:0] from_w_o,
        input read_clk_i,
        output output__,
        input[8:0] input__
    );
    `COCOTB_CODE( std::mem::fifo_impl::fifo_write[1907] )
    logic \write_clk ;
    assign \write_clk  = write_clk_i;
    logic \write_rst ;
    assign \write_rst  = write_rst_i;
    logic[12:0] \mem_mut ;
    assign mem_o = \mem_mut ;
    logic[3:0] \from_w ;
    assign \from_w  = from_w_i;
    logic[3:0] \from_w_mut ;
    assign from_w_o = \from_w_mut ;
    logic \read_clk ;
    assign \read_clk  = read_clk_i;
    (* src = "<compiler dir>/stdlib/mem.spade:202,24" *)
    logic[3:0] _e_1209;
    (* src = "<compiler dir>/stdlib/mem.spade:199,13" *)
    logic[3:0] \read_ptr ;
    localparam[0:0] read_clk_n1 = 0;
    (* src = "<compiler dir>/stdlib/mem.spade:207,27" *)
    logic[8:0] _e_2823;
    (* src = "<compiler dir>/stdlib/mem.spade:207,27" *)
    logic[8:0] _e_2824_mut;
    (* src = "<compiler dir>/stdlib/mem.spade:207,27" *)
    logic[8:0] \write_value ;
    (* src = "<compiler dir>/stdlib/mem.spade:207,27" *)
    logic[8:0] \write_value_mut ;
    localparam[3:0] _e_1220 = 1;
    (* src = "<compiler dir>/stdlib/mem.spade:210,26" *)
    logic[4:0] _e_1218;
    (* src = "<compiler dir>/stdlib/mem.spade:210,20" *)
    logic[3:0] _e_1217;
    (* src = "<compiler dir>/core/ops.spade:53,9" *)
    logic \full ;
    localparam[3:0] _e_1226 = 0;
    (* src = "<compiler dir>/stdlib/mem.spade:212,63" *)
    logic[8:0] _e_1229;
    (* src = "<compiler dir>/stdlib/mem.spade:212,62" *)
    logic[9:0] _e_1228;
    (* src = "<compiler dir>/stdlib/mem.spade:212,56" *)
    logic[9:0] _e_1235;
    (* src = "<compiler dir>/stdlib/mem.spade:213,13" *)
    logic[8:0] _e_1233;
    (* src = "<compiler dir>/stdlib/mem.spade:213,14" *)
    logic[7:0] \v ;
    (* src = "<compiler dir>/stdlib/mem.spade:213,13" *)
    logic _e_1234;
    logic _e_2836;
    localparam[0:0] _e_2837 = 1;
    logic _e_2838;
    logic _e_2839;
    logic _e_2840;
    localparam[3:0] _e_1239 = 1;
    (* src = "<compiler dir>/stdlib/mem.spade:213,39" *)
    logic[4:0] _e_1237;
    (* src = "<compiler dir>/stdlib/mem.spade:213,33" *)
    logic[3:0] _e_1236;
    (* src = "<compiler dir>/stdlib/mem.spade:212,56" *)
    logic[9:0] \_ ;
    localparam[0:0] _e_2842 = 1;
    (* src = "<compiler dir>/stdlib/mem.spade:212,56" *)
    logic[3:0] _e_1227;
    (* src = "<compiler dir>/stdlib/mem.spade:212,24" *)
    reg[3:0] \write_ptr ;
    (* src = "<compiler dir>/stdlib/mem.spade:217,13" *)
    logic[3:0] _e_1242_mut;
    (* src = "<compiler dir>/stdlib/mem.spade:218,13" *)
    logic[8:0] _e_1246_mut;
    (* src = "<compiler dir>/stdlib/mem.spade:218,33" *)
    logic[8:0] _e_1250;
    (* src = "<compiler dir>/stdlib/mem.spade:218,32" *)
    logic[9:0] _e_1249;
    (* src = "<compiler dir>/stdlib/mem.spade:218,26" *)
    logic[9:0] _e_1256;
    (* src = "<compiler dir>/stdlib/mem.spade:219,13" *)
    logic[8:0] _e_1254;
    (* src = "<compiler dir>/stdlib/mem.spade:219,14" *)
    logic[7:0] v_n1;
    (* src = "<compiler dir>/stdlib/mem.spade:219,13" *)
    logic _e_1255;
    logic _e_2865;
    localparam[0:0] _e_2866 = 1;
    logic _e_2867;
    logic _e_2868;
    logic _e_2869;
    (* src = "<compiler dir>/stdlib/mem.spade:219,33" *)
    logic[8:0] _e_1257;
    (* src = "<compiler dir>/stdlib/mem.spade:218,26" *)
    logic[9:0] __n1;
    localparam[0:0] _e_2871 = 1;
    (* src = "<compiler dir>/stdlib/mem.spade:220,18" *)
    logic[8:0] _e_1260;
    (* src = "<compiler dir>/stdlib/mem.spade:218,26" *)
    logic[8:0] _e_1248;
    (* src = "<compiler dir>/stdlib/mem.spade:223,13" *)
    logic[3:0] _e_1262_mut;
    (* src = "<compiler dir>/stdlib/mem.spade:226,20" *)
    logic[8:0] _e_1268_mut;
    (* src = "<compiler dir>/stdlib/mem.spade:224,9" *)
    logic _e_1266;
    (* src = "<compiler dir>/stdlib/mem.spade:224,9" *)
    logic[8:0] _e_1266_mut;
    assign _e_1209 = \from_w [3:0];
    (* src = "<compiler dir>/stdlib/mem.spade:199,13" *)
    
    \std::cdc::sync_uint_counter[1911]  \sync_uint_counter_0 (.source_clk_i(\read_clk ), .dest_clk_i(\write_clk ), .input_i(_e_1209), .output__(\read_ptr ));
    
    assign _e_2823 = _e_2824_mut;
    assign \write_value  = {_e_2823};
    assign {_e_2824_mut} = \write_value_mut ;
    assign _e_1218 = \write_ptr  + _e_1220;
    assign _e_1217 = _e_1218[3:0];
    assign \full  = _e_1217 == \read_ptr ;
    assign _e_1229 = \write_value [8:0];
    assign _e_1228 = {_e_1229, \full };
    
    assign _e_1233 = _e_1235[9:1];
    assign \v  = _e_1233[7:0];
    assign _e_1234 = _e_1235[0];
    assign _e_2836 = _e_1233[8] == 1'd1;
    assign _e_2838 = _e_2836 && _e_2837;
    assign _e_2839 = !_e_1234;
    assign _e_2840 = _e_2838 && _e_2839;
    assign _e_1237 = \write_ptr  + _e_1239;
    assign _e_1236 = _e_1237[3:0];
    
    always_comb begin
        priority casez ({_e_2840, _e_2842})
            2'b1?: _e_1227 = _e_1236;
            2'b01: _e_1227 = \write_ptr ;
            2'b?: _e_1227 = 4'dx;
        endcase
    end
    assign _e_1235 = _e_1228;
    assign \_  = _e_1228;
    always @(posedge \write_clk ) begin
        if (\write_rst ) begin
            \write_ptr  <= _e_1226;
        end
        else begin
            \write_ptr  <= _e_1227;
        end
    end
    assign \mem_mut [12:9] = _e_1242_mut;
    assign _e_1242_mut = \write_ptr ;
    
    assign \mem_mut [8:0] = _e_1246_mut;
    assign _e_1250 = \write_value [8:0];
    assign _e_1249 = {_e_1250, \full };
    
    assign _e_1254 = _e_1256[9:1];
    assign v_n1 = _e_1254[7:0];
    assign _e_1255 = _e_1256[0];
    assign _e_2865 = _e_1254[8] == 1'd1;
    assign _e_2867 = _e_2865 && _e_2866;
    assign _e_2868 = !_e_1255;
    assign _e_2869 = _e_2867 && _e_2868;
    assign _e_1257 = {1'd1, v_n1};
    
    assign _e_1260 = {1'd0, 8'bX};
    always_comb begin
        priority casez ({_e_2869, _e_2871})
            2'b1?: _e_1248 = _e_1257;
            2'b01: _e_1248 = _e_1260;
            2'b?: _e_1248 = 9'dx;
        endcase
    end
    assign _e_1256 = _e_1249;
    assign __n1 = _e_1249;
    assign _e_1246_mut = _e_1248;
    
    assign \from_w_mut [3:0] = _e_1262_mut;
    assign _e_1262_mut = \write_ptr ;
    
    assign \write_value_mut [8:0] = _e_1268_mut;
    assign _e_1266 = {\full };
    assign {_e_1268_mut} = _e_1266_mut;
    assign output__ = _e_1266;
    assign _e_1266_mut = input__;
endmodule


module \std::mem::fifo[1901]  (
        input write_clk_i,
        input write_rst_i,
        input read_clk_i,
        input read_rst_i,
        output[13:0] output__,
        input[9:0] input__
    );
    `COCOTB_CODE( std::mem::fifo[1901] )
    logic \write_clk ;
    assign \write_clk  = write_clk_i;
    logic \write_rst ;
    assign \write_rst  = write_rst_i;
    logic \read_clk ;
    assign \read_clk  = read_clk_i;
    logic \read_rst ;
    assign \read_rst  = read_rst_i;
    (* src = "<compiler dir>/stdlib/mem.spade:239,33" *)
    logic[7:0] _e_1271;
    (* src = "<compiler dir>/stdlib/mem.spade:239,33" *)
    logic[16:0] _e_1271_mut;
    (* src = "<compiler dir>/stdlib/mem.spade:239,9" *)
    logic[12:0] \mem_write_mut ;
    (* src = "<compiler dir>/stdlib/mem.spade:239,9" *)
    logic[7:0] \mem_read ;
    (* src = "<compiler dir>/stdlib/mem.spade:239,9" *)
    logic[3:0] \mem_read_mut ;
    (* src = "<compiler dir>/stdlib/mem.spade:241,26" *)
    logic[3:0] _e_2817;
    (* src = "<compiler dir>/stdlib/mem.spade:241,26" *)
    logic[3:0] _e_2817_mut;
    (* src = "<compiler dir>/stdlib/mem.spade:241,26" *)
    logic[3:0] _e_2818;
    (* src = "<compiler dir>/stdlib/mem.spade:241,26" *)
    logic[3:0] _e_2818_mut;
    (* src = "<compiler dir>/stdlib/mem.spade:241,26" *)
    logic[7:0] _e_1277;
    (* src = "<compiler dir>/stdlib/mem.spade:241,26" *)
    logic[7:0] _e_1277_mut;
    (* src = "<compiler dir>/stdlib/mem.spade:241,9" *)
    logic[3:0] \from_w ;
    (* src = "<compiler dir>/stdlib/mem.spade:241,9" *)
    logic[3:0] \from_w_mut ;
    (* src = "<compiler dir>/stdlib/mem.spade:241,9" *)
    logic[3:0] \to_w ;
    (* src = "<compiler dir>/stdlib/mem.spade:241,9" *)
    logic[3:0] \to_w_mut ;
    (* src = "<compiler dir>/stdlib/mem.spade:243,17" *)
    logic \write ;
    (* src = "<compiler dir>/stdlib/mem.spade:243,17" *)
    logic[8:0] \write_mut ;
    (* src = "<compiler dir>/stdlib/mem.spade:244,16" *)
    logic[12:0] \read ;
    (* src = "<compiler dir>/stdlib/mem.spade:244,16" *)
    logic \read_mut ;
    (* src = "<compiler dir>/stdlib/mem.spade:246,5" *)
    logic[13:0] _e_1295;
    (* src = "<compiler dir>/stdlib/mem.spade:246,5" *)
    logic[9:0] _e_1295_mut;
    (* src = "<compiler dir>/stdlib/mem.spade:239,33" *)
    
    \std::mem::dp_bram[1904]  \dp_bram_0 (.write_clk_i(\write_clk ), .read_clk_i(\read_clk ), .output__(_e_1271), .input__(_e_1271_mut));
    assign _e_1271_mut[16:4] = \mem_write_mut ;
    assign \mem_read  = _e_1271[7:0];
    assign _e_1271_mut[3:0] = \mem_read_mut ;
    
    assign _e_2818 = _e_2817_mut;
    assign _e_2817 = _e_2818_mut;
    assign _e_1277 = {_e_2817, _e_2818};
    assign {_e_2817_mut, _e_2818_mut} = _e_1277_mut;
    assign \from_w  = _e_1277[7:4];
    assign _e_1277_mut[7:4] = \from_w_mut ;
    assign \to_w  = _e_1277[3:0];
    assign _e_1277_mut[3:0] = \to_w_mut ;
    (* src = "<compiler dir>/stdlib/mem.spade:243,17" *)
    
    \std::mem::fifo_impl::fifo_write[1907]  \fifo_write_0 (.write_clk_i(\write_clk ), .write_rst_i(\write_rst ), .mem_o(\mem_write_mut ), .from_w_i(\from_w ), .from_w_o(\from_w_mut ), .read_clk_i(\read_clk ), .output__(\write ), .input__(\write_mut ));
    (* src = "<compiler dir>/stdlib/mem.spade:244,16" *)
    
    \std::mem::fifo_impl::fifo_read[1909]  \fifo_read_0 (.read_clk_i(\read_clk ), .read_rst_i(\read_rst ), .mem_i(\mem_read ), .mem_o(\mem_read_mut ), .to_w_i(\to_w ), .to_w_o(\to_w_mut ), .write_clk_i(\write_clk ), .output__(\read ), .input__(\read_mut ));
    assign _e_1295 = {\write , \read };
    assign {\write_mut , \read_mut } = _e_1295_mut;
    assign output__ = _e_1295;
    assign _e_1295_mut = input__;
endmodule


module \features::mac  (
        input clk_i,
        input[15:0] p_i,
        input[15:0] acc_i,
        output[15:0] output__
    );
    `COCOTB_CODE( features::mac )
    logic \clk ;
    assign \clk  = clk_i;
    logic[15:0] \p ;
    assign \p  = p_i;
    logic[15:0] \acc ;
    assign \acc  = acc_i;
    (* src = "src/main.spade:13,5" *)
    reg[15:0] \s1_p ;
    (* src = "src/main.spade:13,5" *)
    reg[15:0] \s1_acc ;
    (* src = "src/main.spade:13,5" *)
    reg[15:0] \s1_prod ;
    (* src = "src/main.spade:15,5" *)
    reg[15:0] \s2_p ;
    (* src = "src/main.spade:15,5" *)
    reg[15:0] \s2_acc ;
    (* src = "src/main.spade:15,5" *)
    reg[15:0] \s2_prod ;
    (* src = "src/main.spade:15,5" *)
    reg[15:0] \s2_s ;
    (* src = "src/main.spade:12,20" *)
    logic[7:0] _e_1822;
    (* src = "src/main.spade:12,26" *)
    logic[7:0] _e_1824;
    (* src = "src/main.spade:12,20" *)
    logic[15:0] \prod ;
    (* src = "src/main.spade:14,23" *)
    logic[16:0] _e_1828;
    (* src = "src/main.spade:14,17" *)
    logic[15:0] \s ;
    always @(posedge \clk ) begin
        \s1_p  <= \p ;
    end
    always @(posedge \clk ) begin
        \s1_acc  <= \acc ;
    end
    always @(posedge \clk ) begin
        \s1_prod  <= \prod ;
    end
    always @(posedge \clk ) begin
        \s2_p  <= \s1_p ;
    end
    always @(posedge \clk ) begin
        \s2_acc  <= \s1_acc ;
    end
    always @(posedge \clk ) begin
        \s2_prod  <= \s1_prod ;
    end
    always @(posedge \clk ) begin
        \s2_s  <= \s ;
    end
    assign _e_1822 = \p [15:8];
    assign _e_1824 = \p [7:0];
    assign \prod  = _e_1822 * _e_1824;
    assign _e_1828 = \s1_prod  + \s1_acc ;
    assign \s  = _e_1828[15:0];
    assign output__ = \s2_s ;
endmodule


module \features::fsm  (
        input clk_i,
        input rst_i,
        input go_i,
        output[5:0] output__
    );
    `COCOTB_CODE( features::fsm )
    logic \clk ;
    assign \clk  = clk_i;
    logic \rst ;
    assign \rst  = rst_i;
    logic \go ;
    assign \go  = go_i;
    (* src = "src/main.spade:20,27" *)
    logic[5:0] _e_1837;
    (* src = "src/main.spade:20,42" *)
    logic[5:0] _e_1840;
    logic _e_2793;
    localparam[3:0] _e_1845 = 0;
    (* src = "src/main.spade:21,32" *)
    logic[5:0] _e_1844;
    (* src = "src/main.spade:21,55" *)
    logic[5:0] _e_1847;
    (* src = "src/main.spade:21,24" *)
    logic[5:0] _e_1841;
    (* src = "src/main.spade:20,42" *)
    logic[5:0] _e_1849;
    (* src = "src/main.spade:22,9" *)
    logic[3:0] \step ;
    logic _e_2796;
    localparam[0:0] _e_2797 = 1;
    logic _e_2798;
    localparam[3:0] _e_1853 = 15;
    (* src = "<compiler dir>/core/ops.spade:53,9" *)
    logic _e_1851;
    (* src = "src/main.spade:22,45" *)
    logic[5:0] _e_1855;
    localparam[3:0] _e_1861 = 1;
    (* src = "src/main.spade:22,83" *)
    logic[4:0] _e_1859;
    (* src = "src/main.spade:22,77" *)
    logic[3:0] _e_1858;
    (* src = "src/main.spade:22,66" *)
    logic[5:0] _e_1857;
    (* src = "src/main.spade:22,29" *)
    logic[5:0] _e_1850;
    (* src = "src/main.spade:20,42" *)
    logic[5:0] _e_1862;
    logic _e_2801;
    (* src = "src/main.spade:23,24" *)
    logic[5:0] _e_1863;
    (* src = "src/main.spade:20,42" *)
    logic[5:0] _e_1838;
    (* src = "src/main.spade:20,14" *)
    reg[5:0] \s ;
    assign _e_1837 = {2'd0, 4'bX};
    
    assign _e_2793 = _e_1840[5:4] == 2'd0;
    assign _e_1844 = {2'd1, _e_1845};
    assign _e_1847 = {2'd0, 4'bX};
    assign _e_1841 = \go  ? _e_1844 : _e_1847;
    
    assign \step  = _e_1849[3:0];
    assign _e_2796 = _e_1849[5:4] == 2'd1;
    assign _e_2798 = _e_2796 && _e_2797;
    assign _e_1851 = \step  == _e_1853;
    assign _e_1855 = {2'd2, 4'bX};
    assign _e_1859 = \step  + _e_1861;
    assign _e_1858 = _e_1859[3:0];
    assign _e_1857 = {2'd1, _e_1858};
    assign _e_1850 = _e_1851 ? _e_1855 : _e_1857;
    
    assign _e_2801 = _e_1862[5:4] == 2'd2;
    assign _e_1863 = {2'd0, 4'bX};
    always_comb begin
        priority casez ({_e_2793, _e_2798, _e_2801})
            3'b1??: _e_1838 = _e_1841;
            3'b01?: _e_1838 = _e_1850;
            3'b001: _e_1838 = _e_1863;
            3'b?: _e_1838 = 6'dx;
        endcase
    end
    assign _e_1840 = \s ;
    assign _e_1849 = \s ;
    assign _e_1862 = \s ;
    always @(posedge \clk ) begin
        if (\rst ) begin
            \s  <= _e_1837;
        end
        else begin
            \s  <= _e_1838;
        end
    end
    assign output__ = \s ;
endmodule


module main (
        input CLOCK_50,
        input[3:0] KEY,
        input[7:0] SW,
        output[17:0] LEDR,
        output[8:0] LEDG
    );
    `COCOTB_CODE( main )
    logic[17:0] \LEDR_mut ;
    assign LEDR = \LEDR_mut ;
    logic[8:0] \LEDG_mut ;
    assign LEDG = \LEDG_mut ;
    localparam[1:0] _e_1869 = 0;
    (* src = "src/main.spade:36,16" *)
    logic _e_1867;
    (* src = "<compiler dir>/core/ops.spade:301,29" *)
    logic _e_3075;
    (* src = "<compiler dir>/core/ops.spade:301,9" *)
    logic _e_3074;
    (* src = "<compiler dir>/core/ops.spade:301,9" *)
    logic \rst ;
    localparam[26:0] _e_1874 = 0;
    localparam[26:0] _e_1878 = 1;
    (* src = "src/main.spade:37,57" *)
    logic[27:0] _e_1876;
    (* src = "src/main.spade:37,51" *)
    logic[26:0] _e_1875;
    (* src = "src/main.spade:37,19" *)
    reg[26:0] \count ;
    localparam[1:0] _e_1885 = 1;
    (* src = "src/main.spade:38,39" *)
    logic _e_1883;
    (* src = "<compiler dir>/core/ops.spade:301,29" *)
    logic _e_3083;
    (* src = "<compiler dir>/core/ops.spade:301,9" *)
    logic _e_3082;
    (* src = "<compiler dir>/core/ops.spade:301,9" *)
    logic _e_1882;
    (* src = "src/main.spade:38,14" *)
    logic[5:0] \st ;
    (* src = "src/main.spade:39,16" *)
    logic[5:0] _e_1890;
    (* src = "src/main.spade:39,27" *)
    logic[3:0] \_ ;
    logic _e_2804;
    localparam[0:0] _e_2805 = 1;
    logic _e_2806;
    localparam[0:0] _e_1891 = 1;
    (* src = "src/main.spade:39,16" *)
    logic[5:0] __n1;
    localparam[0:0] _e_2808 = 1;
    localparam[0:0] _e_1893 = 0;
    (* src = "src/main.spade:39,16" *)
    logic \busy ;
    (* src = "src/main.spade:41,18" *)
    logic[13:0] _e_1895;
    (* src = "src/main.spade:41,18" *)
    logic[9:0] _e_1895_mut;
    (* src = "src/main.spade:41,9" *)
    logic \w ;
    (* src = "src/main.spade:41,9" *)
    logic[8:0] \w_mut ;
    (* src = "src/main.spade:41,9" *)
    logic[12:0] \r ;
    (* src = "src/main.spade:41,9" *)
    logic \r_mut ;
    (* src = "src/main.spade:42,9" *)
    logic[8:0] _e_1903_mut;
    localparam[1:0] _e_1909 = 2;
    (* src = "src/main.spade:42,23" *)
    logic _e_1907;
    (* src = "<compiler dir>/core/ops.spade:301,29" *)
    logic _e_3091;
    (* src = "<compiler dir>/core/ops.spade:301,9" *)
    logic _e_3090;
    (* src = "<compiler dir>/core/ops.spade:301,9" *)
    logic _e_1906;
    (* src = "src/main.spade:42,32" *)
    logic[8:0] _e_1911;
    (* src = "src/main.spade:42,50" *)
    logic[8:0] _e_1914;
    (* src = "src/main.spade:42,19" *)
    logic[8:0] _e_1905;
    (* src = "src/main.spade:43,9" *)
    logic _e_1916_mut;
    localparam[1:0] _e_1921 = 3;
    (* src = "src/main.spade:43,18" *)
    logic _e_1919;
    (* src = "<compiler dir>/core/ops.spade:301,29" *)
    logic _e_3101;
    (* src = "<compiler dir>/core/ops.spade:301,9" *)
    logic _e_3100;
    (* src = "<compiler dir>/core/ops.spade:301,9" *)
    logic _e_1918;
    (* src = "src/main.spade:44,22" *)
    logic[8:0] _e_1924;
    (* src = "src/main.spade:44,16" *)
    logic[8:0] _e_1927;
    (* src = "src/main.spade:44,31" *)
    logic[7:0] \v ;
    logic _e_2811;
    localparam[0:0] _e_2812 = 1;
    logic _e_2813;
    (* src = "src/main.spade:44,16" *)
    logic[8:0] _e_1929;
    logic _e_2816;
    localparam[7:0] _e_1930 = 0;
    (* src = "src/main.spade:44,16" *)
    logic[7:0] \head ;
    localparam[15:0] _e_1935 = 0;
    (* src = "src/main.spade:46,71" *)
    logic[15:0] _e_1938;
    (* src = "src/main.spade:46,49" *)
    logic[15:0] _e_1936;
    (* src = "src/main.spade:46,19" *)
    reg[15:0] \acc ;
    logic[17:0] _e_1944;
    localparam[26:0] _e_1950 = 23;
    (* src = "src/main.spade:49,15" *)
    logic[26:0] _e_1948;
    (* src = "<compiler dir>/stdlib/conv.spade:512,9" *)
    logic[26:0] \top ;
    localparam[4:0] _e_1956 = 0;
    (* src = "src/main.spade:50,17" *)
    logic _e_1954;
    localparam[4:0] _e_1959 = 1;
    (* src = "src/main.spade:50,25" *)
    logic _e_1957;
    localparam[4:0] _e_1962 = 2;
    (* src = "src/main.spade:50,33" *)
    logic _e_1960;
    localparam[4:0] _e_1965 = 3;
    (* src = "src/main.spade:50,41" *)
    logic _e_1963;
    (* src = "src/main.spade:50,55" *)
    logic _e_1967;
    localparam[0:0] _e_1969 = 0;
    localparam[0:0] _e_1970 = 0;
    localparam[0:0] _e_1971 = 0;
    (* src = "src/main.spade:50,16" *)
    logic[8:0] _e_1953;
    assign _e_1867 = \KEY [_e_1869];
    assign _e_3075 = {_e_1867};
    assign _e_3074 = ~_e_3075;
    assign \rst  = _e_3074;
    assign _e_1876 = \count  + _e_1878;
    assign _e_1875 = _e_1876[26:0];
    always @(posedge \CLOCK_50 ) begin
        if (\rst ) begin
            \count  <= _e_1874;
        end
        else begin
            \count  <= _e_1875;
        end
    end
    assign _e_1883 = \KEY [_e_1885];
    assign _e_3083 = {_e_1883};
    assign _e_3082 = ~_e_3083;
    assign _e_1882 = _e_3082;
    (* src = "src/main.spade:38,14" *)
    
    \features::fsm  \fsm_0 (.clk_i(\CLOCK_50 ), .rst_i(\rst ), .go_i(_e_1882), .output__(\st ));
    
    assign \_  = _e_1890[3:0];
    assign _e_2804 = _e_1890[5:4] == 2'd1;
    assign _e_2806 = _e_2804 && _e_2805;
    
    always_comb begin
        priority casez ({_e_2806, _e_2808})
            2'b1?: \busy  = _e_1891;
            2'b01: \busy  = _e_1893;
            2'b?: \busy  = 1'dx;
        endcase
    end
    assign _e_1890 = \st ;
    assign __n1 = \st ;
    (* src = "src/main.spade:41,18" *)
    
    \std::mem::fifo[1901]  \fifo_0 (.write_clk_i(\CLOCK_50 ), .write_rst_i(\rst ), .read_clk_i(\CLOCK_50 ), .read_rst_i(\rst ), .output__(_e_1895), .input__(_e_1895_mut));
    assign \w  = _e_1895[13];
    assign _e_1895_mut[9:1] = \w_mut ;
    assign \r  = _e_1895[12:0];
    assign _e_1895_mut[0] = \r_mut ;
    assign \w_mut [8:0] = _e_1903_mut;
    assign _e_1907 = \KEY [_e_1909];
    assign _e_3091 = {_e_1907};
    assign _e_3090 = ~_e_3091;
    assign _e_1906 = _e_3090;
    assign _e_1911 = {1'd1, \SW };
    assign _e_1914 = {1'd0, 8'bX};
    assign _e_1905 = _e_1906 ? _e_1911 : _e_1914;
    assign _e_1903_mut = _e_1905;
    
    assign \r_mut  = _e_1916_mut;
    assign _e_1919 = \KEY [_e_1921];
    assign _e_3101 = {_e_1919};
    assign _e_3100 = ~_e_3101;
    assign _e_1918 = _e_3100;
    assign _e_1916_mut = _e_1918;
    
    assign _e_1924 = \r [12:4];
    
    assign \v  = _e_1927[7:0];
    assign _e_2811 = _e_1927[8] == 1'd1;
    assign _e_2813 = _e_2811 && _e_2812;
    
    assign _e_2816 = _e_1929[8] == 1'd0;
    always_comb begin
        priority casez ({_e_2813, _e_2816})
            2'b1?: \head  = \v ;
            2'b01: \head  = _e_1930;
            2'b?: \head  = 8'dx;
        endcase
    end
    assign _e_1927 = _e_1924;
    assign _e_1929 = _e_1924;
    assign _e_1938 = {\SW , \head };
    (* src = "src/main.spade:46,49" *)
    
    \features::mac  \mac_0 (.clk_i(\CLOCK_50 ), .p_i(_e_1938), .acc_i(\acc ), .output__(_e_1936));
    always @(posedge \CLOCK_50 ) begin
        if (\rst ) begin
            \acc  <= _e_1935;
        end
        else begin
            \acc  <= _e_1936;
        end
    end
    assign _e_1944 = {2'b0, \acc };
    assign \LEDR_mut  = _e_1944;
    
    assign _e_1948 = \count  >> _e_1950;
    (* src = "<compiler dir>/stdlib/conv.spade:512,9" *)
    
    \std::conv::impl#15::as_bits[1910]  \as_bits_0 (.self_i(_e_1948), .output__(\top ));
    assign _e_1954 = \top [_e_1956];
    assign _e_1957 = \top [_e_1959];
    assign _e_1960 = \top [_e_1962];
    assign _e_1963 = \top [_e_1965];
    assign _e_1967 = \w ;
    assign _e_1953 = {_e_1971, _e_1970, _e_1969, _e_1967, \busy , _e_1963, _e_1960, _e_1957, _e_1954};
    assign \LEDG_mut  = _e_1953;
    
    
endmodule


module \std::conv::impl#11::as_clock  (
        input self_i,
        output output__
    );
    `COCOTB_CODE( std::conv::impl#11::as_clock )
    logic \self ;
    assign \self  = self_i;
    logic _e_831;
    assign _e_831 = \self ;
    assign output__ = _e_831;
endmodule


module \std::conv::impl#11::as_uint  (
        input self_i,
        output output__
    );
    `COCOTB_CODE( std::conv::impl#11::as_uint )
    logic \self ;
    assign \self  = self_i;
    logic _e_835;
    assign _e_835 = \self ;
    assign output__ = _e_835;
endmodule


module \std::conv::impl#12::as_bool  (
        input self_i,
        output output__
    );
    `COCOTB_CODE( std::conv::impl#12::as_bool )
    logic \self ;
    assign \self  = self_i;
    logic _e_846;
    assign _e_846 = \self ;
    assign output__ = _e_846;
endmodule


module \std::conv::impl#13::as_bool  (
        input self_i,
        output output__
    );
    `COCOTB_CODE( std::conv::impl#13::as_bool )
    logic \self ;
    assign \self  = self_i;
    logic _e_854;
    assign _e_854 = \self ;
    assign output__ = _e_854;
endmodule


module \std::conv::impl#15::as_bits[1910]  (
        input[26:0] self_i,
        output[26:0] output__
    );
    `COCOTB_CODE( std::conv::impl#15::as_bits[1910] )
    logic[26:0] \self ;
    assign \self  = self_i;
    logic[26:0] _e_876;
    assign _e_876 = \self ;
    assign output__ = _e_876;
endmodule


module \std::default::impl#18::default  (
        output output__
    );
    `COCOTB_CODE( std::default::impl#18::default )
    localparam[0:0] _e_930 = 0;
    assign output__ = _e_930;
endmodule


module \std::default::impl#19::default  (
        
    );
    `COCOTB_CODE( std::default::impl#19::default )
    
endmodule


module \std::mem::impl#31::read[1926]  (
        input[7:0] self_i, output[3:0] self_o,
        input read_clk_i,
        input[3:0] addr_i,
        output[7:0] output__
    );
    `COCOTB_CODE( std::mem::impl#31::read[1926] )
    logic[7:0] \self ;
    assign \self  = self_i;
    logic[3:0] \self_mut ;
    assign self_o = \self_mut ;
    logic \read_clk ;
    assign \read_clk  = read_clk_i;
    logic[3:0] \addr ;
    assign \addr  = addr_i;
    (* src = "<compiler dir>/stdlib/mem.spade:62,5" *)
    reg[3:0] \s1_addr ;
    (* src = "<compiler dir>/stdlib/mem.spade:60,13" *)
    logic[3:0] \saddr_mut ;
    (* src = "<compiler dir>/stdlib/mem.spade:60,13" *)
    logic[7:0] \sout ;
    always @(posedge \read_clk ) begin
        \s1_addr  <= \addr ;
    end
    assign \self_mut [3:0] = \saddr_mut ;
    assign \sout  = \self [7:0];
    assign \saddr_mut  = \addr ;
    
    assign output__ = \sout ;
endmodule


module \std::num::impl#33::to_gray[1913]  (
        input[3:0] self_i,
        output[3:0] output__
    );
    `COCOTB_CODE( std::num::impl#33::to_gray[1913] )
    logic[3:0] \self ;
    assign \self  = self_i;
    localparam[3:0] _e_1375 = 1;
    (* src = "<compiler dir>/stdlib/num.spade:251,14" *)
    logic[3:0] _e_1373;
    logic[3:0] \lhs ;
    logic[3:0] \rhs ;
    (* src = "<compiler dir>/core/ops.spade:463,34" *)
    logic[3:0] _e_3120;
    logic[3:0] _e_1372;
    (* src = "<compiler dir>/stdlib/num.spade:251,9" *)
    logic[3:0] _e_1371;
    (* src = "<compiler dir>/stdlib/num.spade:251,14" *)
    
    \core::ops::impl#99::wrapping_shr[1922]  \wrapping_shr_0 (.self_i(\self ), .rhs_i(_e_1375), .output__(_e_1373));
    assign \lhs  = _e_1373;
    assign \rhs  = \self ;
    assign _e_3120 = \lhs  ^ \rhs ;
    assign _e_1372 = _e_3120;
    assign _e_1371 = {_e_1372};
    assign output__ = _e_1371;
endmodule


module \std::num::impl#38::to_uint[1918]  (
        input[3:0] self_i,
        output[3:0] output__
    );
    `COCOTB_CODE( std::num::impl#38::to_uint[1918] )
    logic[3:0] \self ;
    assign \self  = self_i;
    (* src = "<compiler dir>/stdlib/num.spade:378,26" *)
    logic[3:0] _e_1406;
    localparam[2:0] \zeros  = 0;
    localparam[0:0] _e_3130 = 1;
    (* src = "<compiler dir>/stdlib/num.spade:409,51" *)
    logic[3:0] _e_3129;
    logic[3:0] \lhs ;
    logic[3:0] \rhs ;
    (* src = "<compiler dir>/core/ops.spade:375,34" *)
    logic[3:0] _e_3154;
    logic[3:0] \high_bit ;
    localparam[3:0] _e_3134 = 1;
    (* src = "<compiler dir>/stdlib/num.spade:410,58" *)
    logic[3:0] _e_3133;
    logic[3:0] lhs_n1;
    logic[3:0] rhs_n1;
    (* src = "<compiler dir>/core/ops.spade:463,34" *)
    logic[3:0] _e_3186;
    logic[3:0] _e_3132;
    (* src = "<compiler dir>/stdlib/num.spade:410,45" *)
    logic[2:0] \lower_gray ;
    localparam[1:0] zeros_n1 = 0;
    localparam[0:0] _e_3255 = 1;
    (* src = "<compiler dir>/stdlib/num.spade:409,51" *)
    logic[2:0] _e_3254;
    logic[2:0] lhs_n2;
    logic[2:0] rhs_n2;
    (* src = "<compiler dir>/core/ops.spade:375,34" *)
    logic[2:0] _e_3291;
    logic[2:0] high_bit_n1;
    localparam[2:0] _e_3259 = 1;
    (* src = "<compiler dir>/stdlib/num.spade:410,58" *)
    logic[2:0] _e_3258;
    logic[2:0] lhs_n3;
    logic[2:0] rhs_n3;
    (* src = "<compiler dir>/core/ops.spade:463,34" *)
    logic[2:0] _e_3335;
    logic[2:0] _e_3257;
    (* src = "<compiler dir>/stdlib/num.spade:410,45" *)
    logic[1:0] lower_gray_n1;
    localparam[0:0] zeros_n2 = 0;
    localparam[0:0] _e_3428 = 1;
    (* src = "<compiler dir>/stdlib/num.spade:409,51" *)
    logic[1:0] _e_3427;
    logic[1:0] lhs_n4;
    logic[1:0] rhs_n4;
    (* src = "<compiler dir>/core/ops.spade:375,34" *)
    logic[1:0] _e_3476;
    logic[1:0] high_bit_n2;
    localparam[1:0] _e_3432 = 1;
    (* src = "<compiler dir>/stdlib/num.spade:410,58" *)
    logic[1:0] _e_3431;
    logic[1:0] lhs_n5;
    logic[1:0] rhs_n5;
    (* src = "<compiler dir>/core/ops.spade:463,34" *)
    logic[1:0] _e_3532;
    logic[1:0] _e_3430;
    (* src = "<compiler dir>/stdlib/num.spade:410,45" *)
    logic lower_gray_n2;
    logic[1:0] _e_3434;
    logic[1:0] lhs_n6;
    logic[1:0] rhs_n6;
    (* src = "<compiler dir>/core/ops.spade:419,34" *)
    logic[1:0] _e_3688;
    logic[1:0] \result ;
    logic[2:0] _e_3261;
    logic[2:0] lhs_n7;
    logic[2:0] rhs_n7;
    (* src = "<compiler dir>/core/ops.spade:419,34" *)
    logic[2:0] _e_3744;
    logic[2:0] result_n1;
    logic[3:0] _e_3136;
    logic[3:0] lhs_n8;
    logic[3:0] rhs_n8;
    (* src = "<compiler dir>/core/ops.spade:419,34" *)
    logic[3:0] _e_3788;
    logic[3:0] result_n2;
    logic result_n3;
    assign _e_1406 = \self [3:0];
    assign _e_3129 = {_e_3130, \zeros };
    assign \lhs  = _e_1406;
    assign \rhs  = _e_3129;
    assign _e_3154 = \lhs  & \rhs ;
    assign \high_bit  = _e_3154;
    assign _e_3133 = \high_bit  >> _e_3134;
    assign lhs_n1 = _e_1406;
    assign rhs_n1 = _e_3133;
    assign _e_3186 = lhs_n1 ^ rhs_n1;
    assign _e_3132 = _e_3186;
    assign \lower_gray  = _e_3132[2:0];
    assign _e_3254 = {_e_3255, zeros_n1};
    assign lhs_n2 = \lower_gray ;
    assign rhs_n2 = _e_3254;
    assign _e_3291 = lhs_n2 & rhs_n2;
    assign high_bit_n1 = _e_3291;
    assign _e_3258 = high_bit_n1 >> _e_3259;
    assign lhs_n3 = \lower_gray ;
    assign rhs_n3 = _e_3258;
    assign _e_3335 = lhs_n3 ^ rhs_n3;
    assign _e_3257 = _e_3335;
    assign lower_gray_n1 = _e_3257[1:0];
    assign _e_3427 = {_e_3428, zeros_n2};
    assign lhs_n4 = lower_gray_n1;
    assign rhs_n4 = _e_3427;
    assign _e_3476 = lhs_n4 & rhs_n4;
    assign high_bit_n2 = _e_3476;
    assign _e_3431 = high_bit_n2 >> _e_3432;
    assign lhs_n5 = lower_gray_n1;
    assign rhs_n5 = _e_3431;
    assign _e_3532 = lhs_n5 ^ rhs_n5;
    assign _e_3430 = _e_3532;
    assign lower_gray_n2 = _e_3430[0:0];
    assign _e_3434 = {1'b0, lower_gray_n2};
    assign lhs_n6 = high_bit_n2;
    assign rhs_n6 = _e_3434;
    assign _e_3688 = lhs_n6 | rhs_n6;
    assign \result  = _e_3688;
    assign _e_3261 = {1'b0, \result };
    assign lhs_n7 = high_bit_n1;
    assign rhs_n7 = _e_3261;
    assign _e_3744 = lhs_n7 | rhs_n7;
    assign result_n1 = _e_3744;
    assign _e_3136 = {1'b0, result_n1};
    assign lhs_n8 = \high_bit ;
    assign rhs_n8 = _e_3136;
    assign _e_3788 = lhs_n8 | rhs_n8;
    assign result_n2 = _e_3788;
    assign result_n3 = lower_gray_n2;
    assign output__ = result_n2;
endmodule


module \std::option::impl#41::unwrap_or_undef[1920]  (
        input[8:0] self_i,
        output[7:0] output__
    );
    `COCOTB_CODE( std::option::impl#41::unwrap_or_undef[1920] )
    logic[8:0] \self ;
    assign \self  = self_i;
    (* src = "<compiler dir>/stdlib/option.spade:89,9" *)
    logic[8:0] _e_1530;
    (* src = "<compiler dir>/stdlib/option.spade:90,13" *)
    logic[7:0] \t ;
    logic _e_2874;
    localparam[0:0] _e_2875 = 1;
    logic _e_2876;
    (* src = "<compiler dir>/stdlib/option.spade:89,9" *)
    logic[8:0] _e_1532;
    logic _e_2879;
    localparam[7:0] _e_1533 = 8'bx;
    (* src = "<compiler dir>/stdlib/option.spade:89,9" *)
    logic[7:0] _e_1527;
    
    assign \t  = _e_1530[7:0];
    assign _e_2874 = _e_1530[8] == 1'd1;
    assign _e_2876 = _e_2874 && _e_2875;
    
    assign _e_2879 = _e_1532[8] == 1'd0;
    always_comb begin
        priority casez ({_e_2876, _e_2879})
            2'b1?: _e_1527 = \t ;
            2'b01: _e_1527 = _e_1533;
            2'b?: _e_1527 = 8'dx;
        endcase
    end
    assign _e_1530 = \self ;
    assign _e_1532 = \self ;
    assign output__ = _e_1527;
endmodule


module \core::mem::impl#71::read[1916]  (
        input[8:0] self_i, output[5:0] self_o,
        input clk_i,
        input[4:0] req_i,
        output[8:0] output__
    );
    `COCOTB_CODE( core::mem::impl#71::read[1916] )
    logic[8:0] \self ;
    assign \self  = self_i;
    logic[5:0] \self_mut ;
    assign self_o = \self_mut ;
    logic \clk ;
    assign \clk  = clk_i;
    logic[4:0] \req ;
    assign \req  = req_i;
    (* src = "<compiler dir>/core/mem.spade:49,5" *)
    reg[4:0] \s1_req ;
    (* src = "<compiler dir>/core/mem.spade:45,24" *)
    logic[8:0] \res ;
    (* src = "<compiler dir>/core/mem.spade:46,13" *)
    logic _e_1992_mut;
    (* src = "<compiler dir>/core/mem.spade:47,13" *)
    logic[4:0] _e_1996_mut;
    always @(posedge \clk ) begin
        \s1_req  <= \req ;
    end
    assign \res  = \self [8:0];
    assign \self_mut [5] = _e_1992_mut;
    assign _e_1992_mut = \clk ;
    
    assign \self_mut [4:0] = _e_1996_mut;
    assign _e_1996_mut = \req ;
    
    assign output__ = \res ;
endmodule


module \core::mem::impl#73::write[1912]  (
        output[13:0] self_o,
        input clk_i,
        input[12:0] req_i
    );
    `COCOTB_CODE( core::mem::impl#73::write[1912] )
    logic[13:0] \self_mut ;
    assign self_o = \self_mut ;
    logic \clk ;
    assign \clk  = clk_i;
    logic[12:0] \req ;
    assign \req  = req_i;
    (* src = "<compiler dir>/core/mem.spade:64,13" *)
    logic _e_2003_mut;
    (* src = "<compiler dir>/core/mem.spade:65,13" *)
    logic[12:0] _e_2007_mut;
    assign \self_mut [13] = _e_2003_mut;
    assign _e_2003_mut = \clk ;
    
    assign \self_mut [12:0] = _e_2007_mut;
    assign _e_2007_mut = \req ;
    
    
endmodule


module \core::ops::impl#99::wrapping_shr[1922]  (
        input[3:0] self_i,
        input[3:0] rhs_i,
        output[3:0] output__
    );
    `COCOTB_CODE( core::ops::impl#99::wrapping_shr[1922] )
    logic[3:0] \self ;
    assign \self  = self_i;
    logic[3:0] \rhs ;
    assign \rhs  = rhs_i;
    (* src = "<compiler dir>/core/ops.spade:290,9" *)
    logic[3:0] _e_2392;
    assign _e_2392 = \self  >> \rhs ;
    assign output__ = _e_2392;
endmodule

`default_nettype none