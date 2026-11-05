library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;
entity fsm is
  generic (W : positive := 8);
  port (clk, rst : in std_logic;
        din : in std_logic_vector(W-1 downto 0);
        sel : in std_logic_vector(1 downto 0);
        addr : in unsigned(3 downto 0);
        we : in std_logic;
        dout : out std_logic_vector(W-1 downto 0);
        q : out signed(W-1 downto 0);
        bus_io : inout std_logic_vector(3 downto 0);
        st : out std_logic_vector(1 downto 0));
end entity;
architecture rtl of fsm is
  type state_t is (idle, run, done, err);
  signal s : state_t := idle;
  type mem_t is array (0 to 15) of std_logic_vector(W-1 downto 0);
  signal mem : mem_t;
  signal acc : signed(W-1 downto 0);
  signal r : std_logic_vector(W-1 downto 0);
begin
  process (clk, rst)
  begin
    if rst = '1' then
      s <= idle;
      acc <= (others => '0');
    elsif rising_edge(clk) then
      case s is
        when idle => if sel = "01" then s <= run; end if;
        when run => acc <= acc + signed(din); if acc(W-1) = '1' then s <= done; end if;
        when done => acc <= shift_right(acc, 1); s <= err;
        when others => s <= idle;
      end case;
    end if;
  end process;
  process (clk)
  begin
    if rising_edge(clk) then
      if we = '1' then mem(to_integer(addr)) <= din; end if;
      r <= mem(to_integer(addr));
    end if;
  end process;
  with sel select dout <= r when "00", din when "01", std_logic_vector(acc) when "10", (others => '1') when others;
  q <= acc / 3 when sel(0) = '1' else -acc;
  bus_io <= din(3 downto 0) when we = '1' else (others => 'Z');
  st <= std_logic_vector(to_unsigned(state_t'pos(s), 2)) xor bus_io(1 downto 0);
end architecture;
