#!/usr/bin/env python3
"""Rewrite EFFECT_CYCLES in sonido-platform's cycle table from a bench log.

    cd crates/sonido-daisy
    probe-rs run --chip STM32H750IBKx --protocol swd \
        target/thumbv7em-none-eabihf/release/examples/bench_kernels | tee bench.log
    python3 scripts/update_cycle_table.py crates/sonido-daisy/bench.log

Each effect's entry becomes its measured rig-path worst block, rounded up to
the next 1,000 cycles (see the module docs in cycle_table.rs).
"""
import datetime
import pathlib
import re
import sys

TABLE = pathlib.Path(__file__).resolve().parent.parent / "crates/sonido-platform/src/cycle_table.rs"


def main(log_path: str) -> None:
    worst = {
        m.group(1): int(m.group(2))
        for m in re.finditer(r"rig=(\w+) mean=\d+ worst=(\d+)", pathlib.Path(log_path).read_text())
    }
    src = TABLE.read_text()
    start = src.index("pub const EFFECT_CYCLES")
    end = src.index("];", start)
    rows = src[start:end]

    def row(m: re.Match) -> str:
        uid, name = m.group(1), m.group(2)
        if name not in worst:
            sys.exit(f"{name}: not in the bench log")
        w = worst.pop(name)
        return f"({uid}, {-(-w // 1000) * 1000:_}), // {name} — worst {w:,}"

    rows = re.sub(r"\((\d+), [\d_]+\), // (\w+) — worst [\d,]+", row, rows)
    if worst:
        sys.exit(f"not in the table: {sorted(worst)}")
    src = src[:start] + rows + src[end:]
    src = re.sub(
        r"(by `crates/sonido-daisy/examples/bench_kernels.rs`\n//! on )\d{4}-\d{2}-\d{2}",
        lambda m: m.group(1) + datetime.date.today().isoformat(),
        src,
    )
    TABLE.write_text(src)
    print(f"updated {TABLE}")


if __name__ == "__main__":
    main(sys.argv[1])
