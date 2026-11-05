library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity blinky is
    generic (
        -- 26 bits at 50 MHz blinks the top bit about once a second. A
        -- testbench turns this right down so the same design reaches the
        -- same states in microseconds.
        counter_width : positive := 26
    );
    port (
        clk : in  std_logic;
        led : out std_logic_vector(3 downto 0)
    );
end entity blinky;

architecture rtl of blinky is
    signal counter : unsigned(counter_width - 1 downto 0) := (others => '0');
begin
    process (clk)
    begin
        if rising_edge(clk) then
            counter <= counter + 1;
        end if;
    end process;

    led(0) <= counter(counter_width - 1);
    led(1) <= counter(counter_width - 2);
    led(2) <= counter(counter_width - 3);
    led(3) <= counter(counter_width - 4);
end architecture rtl;
