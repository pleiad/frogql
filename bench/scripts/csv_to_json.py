#!/usr/bin/env python3
"""Convert Text2GQL Spanner Instance CSVs to gqlrust JSON format.

Reads spanner_import_config.json to determine which files are nodes vs edges,
and produces a single JSON file loadable by Graph::from_json_str.

Usage: python csv_to_json.py <spanner_instance_dir> <output.json>
"""

import csv
import json
import os
import re
import sys


def infer_edge_label(filename):
    """Extract edge label from filename like PersonACTED_INMovie → ACTED_IN."""
    name = os.path.splitext(filename)[0]
    # Pattern: <SrcType><LABEL><DstType>
    # The label is the uppercase/underscore part between two CamelCase words
    match = re.match(r'^[A-Z][a-z]+([A-Z_]+[A-Z])[A-Z][a-z]+', name)
    if match:
        return match.group(1)
    # Fallback: everything after first lowercase letter sequence
    parts = re.split(r'(?<=[a-z])(?=[A-Z_])', name, maxsplit=1)
    if len(parts) > 1:
        label = parts[1]
        # Remove trailing node type (CamelCase at end)
        label = re.sub(r'[A-Z][a-z]+$', '', label)
        return label
    return name


def load_config(config_path):
    with open(config_path) as f:
        return json.load(f)


def is_edge_file(file_config):
    """An edge file has SRC_ID and DST_ID columns."""
    cols = file_config.get("columns", {})
    return "SRC_ID" in cols and "DST_ID" in cols


def read_csv_rows(csv_path):
    with open(csv_path, newline='', encoding='utf-8') as f:
        reader = csv.DictReader(f)
        return list(reader)


def convert_value(val, col_type):
    """Convert string CSV value to typed Python value."""
    if val is None or val == '':
        return None
    if col_type == "INT64":
        try:
            return int(val)
        except ValueError:
            try:
                return int(float(val))
            except ValueError:
                return val
    elif col_type == "BOOL":
        return val.lower() in ('true', '1', 'yes')
    elif col_type == "FLOAT64":
        try:
            return float(val)
        except ValueError:
            return val
    return val  # STRING


def main():
    if len(sys.argv) < 3:
        print(f"Usage: {sys.argv[0]} <spanner_instance_dir> <output.json>")
        sys.exit(1)

    instance_dir = sys.argv[1]
    output_path = sys.argv[2]

    config_path = os.path.join(instance_dir, "spanner_import_config.json")
    if not os.path.exists(config_path):
        print(f"Error: {config_path} not found")
        sys.exit(1)

    config = load_config(config_path)

    nodes = []
    edges = []
    node_ids = set()

    for file_config in config["files"]:
        csv_path = os.path.join(instance_dir, file_config["path"])
        if not os.path.exists(csv_path):
            print(f"  Warning: {csv_path} not found, skipping")
            continue

        cols = file_config.get("columns", {})
        rows = read_csv_rows(csv_path)

        if is_edge_file(file_config):
            # Edge file
            label = infer_edge_label(file_config["path"])
            prop_cols = {k: v for k, v in cols.items()
                        if k not in ("SRC_ID", "DST_ID", "vid")}

            for row in rows:
                src = row.get("SRC_ID", "").strip()
                dst = row.get("DST_ID", "").strip()
                eid = row.get("vid", f"e_{src}_{dst}_{label}").strip()

                props = {}
                for col_name, col_type in prop_cols.items():
                    val = row.get(col_name)
                    if val is not None and val != '':
                        converted = convert_value(val, col_type)
                        if converted is not None:
                            props[col_name] = converted

                edges.append({
                    "id": eid,
                    "labels": [label],
                    "props": props,
                    "endpoints": [src, dst],
                    "directionality": "->"
                })
        else:
            # Node file
            label = file_config.get("label", os.path.splitext(file_config["path"])[0])
            prop_cols = {k: v for k, v in cols.items() if k != "vid"}

            for row in rows:
                nid = row.get("vid", "").strip()
                if nid in node_ids:
                    # Duplicate node ID — merge labels
                    for existing in nodes:
                        if existing["id"] == nid:
                            if label not in existing["labels"]:
                                existing["labels"].append(label)
                            break
                    continue

                props = {}
                for col_name, col_type in prop_cols.items():
                    val = row.get(col_name)
                    if val is not None and val != '':
                        converted = convert_value(val, col_type)
                        if converted is not None:
                            props[col_name] = converted

                nodes.append({
                    "id": nid,
                    "labels": [label],
                    "props": props
                })
                node_ids.add(nid)

    graph = {"nodes": nodes, "edges": edges}

    with open(output_path, 'w') as f:
        json.dump(graph, f, indent=2)

    print(f"Converted: {len(nodes)} nodes, {len(edges)} edges → {output_path}")


if __name__ == "__main__":
    main()
