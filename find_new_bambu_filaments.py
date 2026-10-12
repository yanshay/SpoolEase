#!/usr/bin/env python3
"""

Checks BambuStudio's filaments_color_codes.json against SpoolEase's CSV files:
- core/data/base-filaments-index.csv
- core/data/bambu-color-names.csv
and the web interface (defaultFilamentListCsv in inventory.js).

Prints any missing filament types or colors that need to be added.
"""

import argparse
import csv
import json
import urllib.parse
import urllib.request
from pathlib import Path

DEFAULT_URL = (
    "https://raw.githubusercontent.com/bambulab/BambuStudio/refs/heads/master/"
    "resources/profiles/BBL/filament/filaments_color_codes.json"
)

BAMBU_PROFILES_BASE_URL = (
    "https://raw.githubusercontent.com/bambulab/BambuStudio/master/resources/profiles/BBL/filament/"
)

COMMON_FAMILIES = ("PETG", "PLA", "ABS", "ASA", "TPU", "PC", "PA")
INHERITS_MATERIAL_MAP = {
    "PET": "PETG",
    "PLA": "PLA",
    "ABS": "ABS",
    "ASA": "ASA",
    "PC": "PC",
    "TPU": "TPU",
    "PA": "PA",
}


def find_repo_root() -> Path:
    """Finds the root repository directory containing core/data."""
    for start in (Path(__file__).resolve().parent, Path.cwd().resolve()):
        for p in (start, *start.parents):
            if (p / "core" / "data" / "base-filaments-index.csv").is_file():
                return p
    return Path.cwd().resolve()


def download_or_load_json(url: str, local_path: Path, force_local: bool = False) -> dict:
    """Downloads filaments_color_codes.json or falls back to local file."""
    if not force_local and url:
        print(f"Fetching filament definitions from:\n  {url}")
        try:
            req = urllib.request.Request(url, headers={"User-Agent": "SpoolEase/1.0"})
            with urllib.request.urlopen(req, timeout=15) as resp:
                data = json.loads(resp.read().decode("utf-8"))
                print(f"Downloaded successfully ({len(data.get('data', []))} records).\n")
                return data
        except Exception as err:
            print(f"[!] Failed to download from URL: {err}")
            print(f"    Falling back to local file: {local_path}\n")

    if local_path.is_file():
        print(f"Loading local filament definitions from:\n  {local_path}")
        with open(local_path, "r", encoding="utf-8") as f:
            data = json.load(f)
            print(f"Loaded successfully ({len(data.get('data', []))} records).\n")
            return data

    raise FileNotFoundError(f"Could not load filament data from URL or local path: {local_path}")


def load_materials_csv(repo_root: Path) -> dict:
    """Loads core/data/materials.csv mapping material -> {temp_min, temp_high}."""
    path = repo_root / "core" / "data" / "materials.csv"
    materials = {}
    if not path.is_file():
        return materials

    with open(path, "r", encoding="utf-8") as f:
        reader = csv.DictReader(f)
        for row in reader:
            mat = row.get("material", "").strip()
            if mat:
                materials[mat] = {
                    "temp_min": int(row.get("temp_min", 200)),
                    "temp_high": int(row.get("temp_high", 240)),
                }
    return materials


def load_base_filaments_index(repo_root: Path) -> tuple[set, list]:
    """Loads core/data/base-filaments-index.csv returning existing IDs and rows."""
    path = repo_root / "core" / "data" / "base-filaments-index.csv"
    fila_ids = set()
    rows = []
    if not path.is_file():
        return fila_ids, rows

    with open(path, "r", encoding="utf-8") as f:
        for line in f:
            raw = line.strip()
            if not raw or raw.startswith("#"):
                continue
            parts = [p.strip() for p in raw.split(",")]
            if parts:
                fila_ids.add(parts[0])
                rows.append(parts)
    return fila_ids, rows


def load_bambu_color_names(repo_root: Path) -> tuple[set, set, list]:
    """Loads core/data/bambu-color-names.csv returning (id_codes, id_colors, rows)."""
    path = repo_root / "core" / "data" / "bambu-color-names.csv"
    id_code_set = set()
    id_color_set = set()
    rows = []
    if not path.is_file():
        return id_code_set, id_color_set, rows

    with open(path, "r", encoding="utf-8") as f:
        for line in f:
            raw = line.strip()
            if not raw or raw.startswith("#"):
                continue
            parts = [p.strip() for p in raw.split(",")]
            if len(parts) >= 4:
                fid = parts[0]
                color_hex = parts[1].upper()
                code = parts[3]
                id_code_set.add((fid, code))
                id_color_set.add((fid, color_hex))
                rows.append(parts)
    return id_code_set, id_color_set, rows


def fetch_bambu_json_file(filename: str) -> dict | None:
    """Fetches a JSON file from BambuStudio profiles directory."""
    if not filename.endswith(".json"):
        filename = f"{filename}.json"
    url = f"{BAMBU_PROFILES_BASE_URL}{urllib.parse.quote(filename)}"
    try:
        req = urllib.request.Request(url, headers={"User-Agent": "SpoolEase/1.0"})
        with urllib.request.urlopen(req, timeout=10) as resp:
            return json.loads(resp.read().decode("utf-8"))
    except Exception:
        return None


def _parse_first_int(val) -> int | None:
    """Extracts first element of list as an integer if available."""
    if val and isinstance(val, list) and val[0]:
        try:
            return int(val[0])
        except (ValueError, TypeError):
            pass
    return None


def fetch_printer_profile_temps(display_name: str) -> tuple[int | None, int | None, str | None]:
    """
    Fetches printer profile from BambuStudio GitHub repo:
      .../resources/profiles/BBL/filament/{display_name} @base.json
    Follows 'inherits' hierarchy to resolve missing temperatures and material.
    """
    prof = fetch_bambu_json_file(f"{display_name} @base.json")
    if not prof:
        return None, None, None

    min_temp = None
    max_temp = None
    material = None

    curr_prof = prof
    for _ in range(4):
        if min_temp is None:
            min_temp = _parse_first_int(curr_prof.get("nozzle_temperature_range_low"))
        if max_temp is None:
            max_temp = _parse_first_int(curr_prof.get("nozzle_temperature_range_high"))

        if material is None:
            filament_type = curr_prof.get("filament_type")
            if filament_type and isinstance(filament_type, list) and filament_type[0]:
                material = str(filament_type[0]).strip()
            elif "inherits" in curr_prof:
                inherits_upper = str(curr_prof["inherits"]).upper()
                for key, mat in INHERITS_MATERIAL_MAP.items():
                    if key in inherits_upper:
                        material = mat
                        break

        if min_temp is not None and max_temp is not None and material is not None:
            break

        parent = curr_prof.get("inherits")
        if not parent:
            break
        curr_prof = fetch_bambu_json_file(parent)
        if not curr_prof:
            break

    return min_temp, max_temp, material


def infer_material(fila_type: str, profile_material: str | None, materials_map: dict) -> str:
    """Infers filament material using profile material, materials.csv, or family keywords."""
    if profile_material:
        return profile_material

    upper_type = fila_type.upper()
    # Check materials.csv first (longest match)
    for mat in sorted(materials_map.keys(), key=len, reverse=True):
        if mat.upper() in upper_type:
            return mat

    # Fallback to standard base families
    for family in COMMON_FAMILIES:
        if family in upper_type:
            return family

    return fila_type.split()[0].upper() if fila_type else "UNKNOWN"


def get_filament_info(
    fila_id: str,
    fila_type: str,
    materials_map: dict,
    warnings_list: list,
) -> tuple[str, str, int, int, str]:
    """Returns (fila_id, display_name, min_temp, max_temp, material), defaulting to 123 on failure."""
    display_name = fila_type if fila_type.startswith("Bambu ") else f"Bambu {fila_type}"
    min_temp, max_temp, prof_mat = fetch_printer_profile_temps(display_name)
    material = infer_material(fila_type, prof_mat, materials_map)

    if min_temp is None or max_temp is None:
        min_temp = 123
        max_temp = 123
        warnings_list.append(
            f"WARNING: Could not fetch printer profile or temperature range for '{display_name}' ({fila_id}). "
            f"Temperatures set to 123. Low and max temps need to be set manually."
        )

    return fila_id, display_name, min_temp, max_temp, material


def format_hex_colors(color_list: list) -> str:
    """Strips '#' and joins color hex values with '/' in uppercase."""
    return "/".join(c.lstrip("#").upper() for c in color_list)


def main():
    parser = argparse.ArgumentParser(
        description="Check BambuStudio filaments_color_codes.json against SpoolEase CSV files and web interface."
    )
    parser.add_argument("--url", default=DEFAULT_URL, help="URL to filaments_color_codes.json")
    parser.add_argument("--file", help="Explicit path to local filaments_color_codes.json")
    parser.add_argument("--local", action="store_true", help="Force using local filaments_color_codes.json")
    args = parser.parse_args()

    repo_root = find_repo_root()
    local_json_path = Path(args.file) if args.file else (repo_root / "filaments_color_codes.json")

    data = download_or_load_json(url=args.url, local_path=local_json_path, force_local=args.local)
    records = data.get("data", [])

    materials_map = load_materials_csv(repo_root)
    existing_base_ids, _ = load_base_filaments_index(repo_root)
    existing_id_codes, _, _ = load_bambu_color_names(repo_root)

    # 1. Identify missing base filaments
    missing_base_filaments = {}
    warnings_list = []
    for item in records:
        fid = item.get("fila_id")
        if not fid:
            continue
        if fid not in existing_base_ids and fid not in missing_base_filaments:
            ftype = item.get("fila_type", "")
            fid, dname, min_t, max_t, mat = get_filament_info(
                fid, ftype, materials_map, warnings_list
            )
            missing_base_filaments[fid] = {
                "fila_id": fid,
                "display_name": dname,
                "min_temp": min_t,
                "max_temp": max_t,
                "material": mat,
                "line": f"{fid},{dname},{min_t},{max_t},{mat}",
            }

    # 2. Identify missing color entries
    missing_color_entries = []
    seen_missing_colors = set()
    for item in records:
        fid = item.get("fila_id")
        code = str(item.get("fila_color_code", ""))
        if not fid or not code:
            continue

        if (fid, code) not in existing_id_codes:
            key = (fid, code)
            if key in seen_missing_colors:
                continue
            seen_missing_colors.add(key)

            hex_color = format_hex_colors(item.get("fila_color", []))
            name_obj = item.get("fila_color_name")
            color_name = ""
            if isinstance(name_obj, dict):
                color_name = name_obj.get("en", "") or name_obj.get("zh", "")
            elif isinstance(name_obj, str):
                color_name = name_obj

            missing_color_entries.append({
                "fila_id": fid,
                "hex": hex_color,
                "name": color_name,
                "code": code,
                "line": f"{fid},{hex_color},{color_name},{code}",
            })

    # Print results
    print("=" * 80)
    print("SPOOLEASE BAMBU FILAMENT UPDATE CHECK")
    print("=" * 80)
    print(f"Total records in JSON:          {len(records)}")
    print(f"Existing base filaments:        {len(existing_base_ids)}")
    print(f"Existing color entries:         {len(existing_id_codes)}")
    print(f"Missing base filaments found:   {len(missing_base_filaments)}")
    print(f"Missing color entries found:    {len(missing_color_entries)}")
    print("=" * 80)

    if warnings_list:
        print("\nWARNINGS:")
        print("-" * 80)
        for w in warnings_list:
            print(f"! {w}")

    if missing_base_filaments:
        print("\n[1] ADD TO `core/data/base-filaments-index.csv` and the web interface `defaultFilamentListCsv` in `inventory.js`:")
        print("-" * 80)
        for entry in missing_base_filaments.values():
            print(entry["line"])
    else:
        print("\n[+] Base filaments index & web interface are up to date.")

    if missing_color_entries:
        print("\n[2] ADD TO `core/data/bambu-color-names.csv`:")
        print("-" * 80)
        for entry in missing_color_entries:
            print(entry["line"])
    else:
        print("\n[+] Bambu color names are up to date.")

    print("\n" + "=" * 80)
    print("CHECK COMPLETE")
    print("=" * 80)


if __name__ == "__main__":
    main()
