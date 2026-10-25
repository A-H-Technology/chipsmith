-- GHDL idioms: signed multiply, variables in a clocked process with a loop,
-- a case with ranges and choices, a RAM with initial contents read in
-- another process, a function over a range.
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;
entity ops is
  generic (N : positive := 4);
  port (clk, rst : in std_logic;
        a, b : in signed(7 downto 0);
        sel : in unsigned(2 downto 0);
        din : in std_logic_vector(7 downto 0);
        wa, ra : in unsigned(3 downto 0);
        we : in std_logic;
        prod : out signed(15 downto 0);
        cnt : out unsigned(N-1 downto 0);
        y : out std_logic_vector(7 downto 0);
        q : out std_logic_vector(7 downto 0);
        par : out std_logic);
end entity;
architecture rtl of ops is
  type ram_t is array (0 to 15) of std_logic_vector(7 downto 0);
  signal ram : ram_t := (others => x"5a");
  signal c : unsigned(N-1 downto 0);
  function parity(v : std_logic_vector) return std_logic is
    variable p : std_logic := '0';
  begin
    for i in v'range loop
      p := p xor v(i);
    end loop;
    return p;
  end function;
begin
  prod <= a * b;
  process (clk, rst)
    variable acc : unsigned(N-1 downto 0);
  begin
    if rst = '1' then
      c <= (others => '0');
    elsif rising_edge(clk) then
      acc := c;
      for i in 0 to 2 loop
        if sel(i) = '1' then
          acc := acc + 1;
        end if;
      end loop;
      c <= acc;
    end if;
  end process;
  cnt <= c;
  process (sel, din, a)
  begin
    case to_integer(sel) is
      when 0 | 1 => y <= din;
      when 2 => y <= std_logic_vector(a);
      when 3 to 5 => y <= not din;
      when others => y <= (others => '0');
    end case;
  end process;
  process (clk)
  begin
    if rising_edge(clk) then
      if we = '1' then
        ram(to_integer(wa)) <= din;
      end if;
    end if;
  end process;
  process (clk)
  begin
    if rising_edge(clk) then
      q <= ram(to_integer(ra));
    end if;
  end process;
  par <= parity(din);
end architecture;
