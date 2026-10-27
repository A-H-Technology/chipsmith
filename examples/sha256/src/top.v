// Joachim Strömbergson's (secworks) SHA-256 core driven through its
// register interface (sha256.v): a sequencer writes a 512-bit block (a
// running seed, the word index and SW), starts a SHA-256 hash, waits for
// ready, reads the eight digest words and XORs them together. LEDR and
// LEDG show the fold; every hash takes about 90 cycles. KEY[0] (active
// low) resets.
module top(input CLOCK_50, input [3:0] KEY, input [17:0] SW,
           output [17:0] LEDR, output [8:0] LEDG);
  wire clk = CLOCK_50;
  reg [1:0] rst_sync = 2'b00;
  always @(posedge clk) rst_sync <= {rst_sync[0], KEY[0]};
  wire reset_n = rst_sync[1];

  localparam WAIT = 5'd17, POLL = 5'd21, DIGEST = 5'd22, LAST = 5'd29;
  reg [4:0] step = 0;
  reg [31:0] seed = 0;
  reg [31:0] fold = 0;
  reg [31:0] result = 0;

  reg cs, we;
  reg [7:0] address;
  reg [31:0] write_data;
  wire [31:0] read_data;
  wire error;

  always @* begin
    cs = 0; we = 0; address = 0; write_data = 0;
    if (step < 16) begin
      cs = 1; we = 1; address = {4'h1, step[3:0]};
      write_data = seed ^ {step[3:0], 10'b0, SW};
    end else if (step == 16) begin
      cs = 1; we = 1; address = 8'h08; write_data = 32'h5; // init, SHA-256
    end else if (step == POLL) begin
      cs = 1; address = 8'h09;
    end else if (step >= DIGEST) begin
      cs = 1; address = 8'h20 + (step - DIGEST);
    end
  end

  always @(posedge clk)
    if (!reset_n) begin
      step <= 0; seed <= 0; fold <= 0;
    end else if (step == POLL) begin
      if (read_data[0]) step <= DIGEST;
    end else if (step == LAST) begin
      result <= fold ^ read_data;
      fold <= 0;
      seed <= seed + 1'b1;
      step <= 0;
    end else begin
      if (step >= DIGEST) fold <= fold ^ read_data;
      step <= step + 1'b1;
    end

  sha256 sha (.clk(clk), .reset_n(reset_n), .cs(cs), .we(we), .address(address),
              .write_data(write_data), .read_data(read_data), .error(error));

  assign LEDR = result[17:0] ^ result[31:14];
  assign LEDG = {error, result[31:24]};
endmodule
