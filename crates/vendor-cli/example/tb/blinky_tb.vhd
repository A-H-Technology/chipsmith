library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;
use std.env.finish;

entity blinky_tb is
end entity blinky_tb;

architecture sim of blinky_tb is
    -- Small enough that the top bits move in a few hundred cycles. The design
    -- is the same one the board gets at 26 bits; only the time scale changes.
    constant counter_width : positive := 6;
    constant wrap          : positive := 2 ** counter_width;
    -- the 50 MHz [clocks] constrains the design to
    constant period        : time     := 20 ns;

    signal clk : std_logic := '0';
    signal led : std_logic_vector(3 downto 0);

    -- What the LEDs should show after `cycles` rising edges, stated in terms
    -- of the clock rather than the design's own counter: led(0) is the slowest
    -- bit, and each one after it blinks twice as fast.
    function expected_led (cycles : natural) return std_logic_vector is
        variable count : unsigned(counter_width - 1 downto 0) :=
            to_unsigned(cycles mod wrap, counter_width);
        variable leds  : std_logic_vector(3 downto 0);
    begin
        for i in leds'range loop
            leds(i) := count(counter_width - 1 - i);
        end loop;
        return leds;
    end function;
begin
    dut : entity work.blinky
        generic map (counter_width => counter_width)
        port map (clk => clk, led => led);

    clk <= not clk after period / 2;

    check : process
    begin
        assert counter_width >= 4
            report "blinky drives four LEDs, so the counter needs at least four bits"
            severity failure;

        -- the concurrent assignments have not run at time zero, so settle a
        -- delta first; this is still well before the first rising edge
        wait for period / 4;
        assert led = "0000"
            report "led should be clear before the first clock edge, was " & to_string(led)
            severity error;

        -- Two full wraps: one to see every LED pattern, the second to prove the
        -- counter rolls over instead of saturating.
        for cycle in 1 to 2 * wrap loop
            wait until rising_edge(clk);
            -- past the delta cycles, so led has settled for this edge
            wait for period / 4;

            assert led = expected_led(cycle)
                report "cycle " & integer'image(cycle) & ": led was " & to_string(led)
                     & ", expected " & to_string(expected_led(cycle))
                severity error;
        end loop;

        report "blinky_tb: " & integer'image(2 * wrap) & " cycles checked";
        finish;
    end process;
end architecture sim;
