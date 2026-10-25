-- GHDL idioms beyond blinky: a sub-entity instantiated with two generic
-- values (separate Verilog modules), a constant ROM, for-generate, signed
-- arithmetic with resize and shifts, a synchronous reset.
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity acc is
  generic (W : positive := 4; STEP : integer := 1);
  port (clk, rst : in std_logic;
        en : in std_logic;
        q : out unsigned(W-1 downto 0));
end entity;

architecture rtl of acc is
  signal r : unsigned(W-1 downto 0) := (others => '0');
begin
  process (clk)
  begin
    if rising_edge(clk) then
      if rst = '1' then
        r <= (others => '0');
      elsif en = '1' then
        r <= r + to_unsigned(STEP, W);
      end if;
    end if;
  end process;
  q <= r;
end architecture;

library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity rom is
  port (clk, rst : in std_logic;
        sw : in std_logic_vector(7 downto 0);
        led : out std_logic_vector(15 downto 0);
        s : out signed(9 downto 0));
end entity;

architecture rtl of rom is
  type rom_t is array (0 to 15) of std_logic_vector(7 downto 0);
  constant table : rom_t := (
    x"01", x"23", x"45", x"67", x"89", x"ab", x"cd", x"ef",
    x"fe", x"dc", x"ba", x"98", x"76", x"54", x"32", x"10");
  signal a, b : unsigned(5 downto 0);
  signal word : std_logic_vector(7 downto 0);
  signal par : std_logic_vector(3 downto 0);
begin
  u0 : entity work.acc generic map (W => 6, STEP => 1) port map (clk, rst, sw(0), a);
  u1 : entity work.acc generic map (W => 6, STEP => 3) port map (clk, rst, sw(1), b);
  process (clk)
  begin
    if rising_edge(clk) then
      word <= table(to_integer(a(3 downto 0)));
    end if;
  end process;
  g : for i in 0 to 3 generate
    par(i) <= word(2*i) xor word(2*i+1) xor sw(i+4);
  end generate;
  led <= word & par & std_logic_vector(b(3 downto 0));
  s <= shift_left(resize(signed(sw), 10), 1) - signed(resize(a, 10)) + shift_right(signed(resize(b, 10)), 2);
end architecture;
