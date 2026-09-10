#!/usr/bin/env python3
"""Summarize completed native GPU campaigns, retaining individual samples.

GPU CSV timestamps are filtered to the measured interval, excluding its first
and last second. Old campaigns may provide wall-monotonic-calibration.json.
GPU metrics describe the entire board, including the producer and other apps.
"""
import argparse
import csv
import datetime
import json
import math
from pathlib import Path
import random
import statistics


def distribution(values):
    return {"n": len(values), "median": statistics.median(values), "mean": statistics.mean(values),
            "min": min(values), "max": max(values), "values": values}


def board_metrics(root, sample, utc_offset):
    calibration_path = root / "wall-monotonic-calibration.json"
    calibration = json.loads(calibration_path.read_text()) if calibration_path.exists() else None
    offset = calibration["wall_ns"] - calibration["monotonic_ns"] if calibration else None
    start = sample["before"].get("wall_ns")
    end = sample["after"].get("wall_ns")
    if start is None or end is None:
        if offset is None:
            return {"unavailable": "no mapping between telemetry wall time and measurement clock"}
        start = sample["before"]["monotonic_ns"] + offset
        end = sample["after"]["monotonic_ns"] + offset
    timezone = datetime.timezone(datetime.timedelta(seconds=utc_offset))
    boards = {}
    with (root / sample["directory"] / "gpu.csv").open() as stream:
        for row in csv.reader(stream):
            if len(row) != 10:
                continue
            stamp = datetime.datetime.strptime(row[0].strip(), "%Y/%m/%d %H:%M:%S.%f").replace(tzinfo=timezone).timestamp() * 1e9
            if not start + 1e9 <= stamp <= end - 1e9:
                continue
            values = boards.setdefault(row[2].strip(), {})
            for index, metric in ((3, "utilization_percent"), (5, "power_w"), (6, "graphics_mhz"), (7, "memory_mhz"), (8, "temperature_c"), (9, "memory_mib")):
                try:
                    value = float(row[index])
                except ValueError:
                    continue
                if math.isfinite(value):
                    values.setdefault(metric, []).append(value)
    return {uuid: {key: distribution(values) for key, values in data.items()} for uuid, data in boards.items()}


def paired_changes(samples, reference, candidate, metric):
    indexed = {(sample["case"], sample["policy"], sample["run"], sample["label"]): sample for sample in samples}
    if len(indexed) != len(samples):
        raise ValueError("duplicate campaign cell; paired samples would be overwritten")
    results = {}
    for case, policy in sorted({(sample["case"], sample["policy"]) for sample in samples}):
        changes = []
        for run in sorted({sample["run"] for sample in samples}):
            before = indexed.get((case, policy, run, reference))
            after = indexed.get((case, policy, run, candidate))
            if before and after and before[metric]:
                changes.append((after[metric] / before[metric] - 1) * 100)
        if not changes:
            continue
        rng = random.Random(1729)
        means = sorted(statistics.mean(rng.choices(changes, k=len(changes))) for _ in range(10000))
        results[f"{case}/{policy}"] = {"relative_change_percent": distribution(changes),
            "paired_mean_bootstrap_95_percent": [means[249], means[9749]],
            "method": "10000 paired bootstrap resamples, seed 1729; small-sample uncertainty remains"}
    return results


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("campaign", type=Path)
    parser.add_argument("--utc-offset-seconds", type=int, required=True, help="timezone used by nvidia-smi timestamps during this campaign")
    parser.add_argument("--reference", default="baseline")
    parser.add_argument("--candidate", default="final")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    samples = json.loads((args.campaign / "samples.json").read_text())
    metadata = json.loads((args.campaign / "metadata.json").read_text())
    expected = len(metadata["binaries"]) * metadata["arguments"]["runs"] * len(metadata["arguments"]["cases"]) * len(metadata["arguments"]["policies"])
    if len(samples) != expected or not (args.campaign / "summary.json").exists():
        raise SystemExit(f"campaign incomplete: {len(samples)} of {expected} samples")
    for sample in samples:
        feedback = sample["presentation"]
        sample["presentation_fps"] = feedback["fps"] if feedback else 0
        sample["missed_refreshes"] = feedback["estimated_missed_refreshes"] if feedback else 0
        sample["cpu_us_per_presented_frame"] = sample["cpu_percent"] / 100 * sample["seconds"] * 1e6 / feedback["count"] if feedback else 0
        sample["boards"] = board_metrics(args.campaign, sample, args.utc_offset_seconds)
    summary = {}
    for label, case, policy in sorted({(sample["label"], sample["case"], sample["policy"]) for sample in samples}):
        group = [sample for sample in samples if (sample["label"], sample["case"], sample["policy"]) == (label, case, policy)]
        summary[f"{label}/{case}/{policy}"] = {metric: distribution([sample[metric] for sample in group])
            for metric in ("cpu_percent", "presentation_fps", "cpu_us_per_presented_frame", "render_wall_us_per_frame", "missed_refreshes")}
    result = {"metadata": metadata, "summary": summary, "samples": samples,
              "cpu_comparison": paired_changes(samples, args.reference, args.candidate, "cpu_percent"),
              "cpu_per_frame_comparison": paired_changes(samples, args.reference, args.candidate, "cpu_us_per_presented_frame")}
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({"summary": summary, "cpu_comparison": result["cpu_comparison"]}, indent=2))


if __name__ == "__main__":
    main()
