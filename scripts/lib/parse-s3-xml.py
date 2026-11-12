#!/usr/bin/env python3
"""
S3 XML Response Parser

Parses Amazon S3 / MinIO / SeaweedFS / RustFS XML responses from standard input.
Provides extracted fields for keys, continuation tokens, and truncation status.

Example usage:
  curl ... | ./scripts/lib/parse-s3-xml.py --keys
  curl ... | ./scripts/lib/parse-s3-xml.py --is-truncated
  curl ... | ./scripts/lib/parse-s3-xml.py --next-token
"""

import sys
import xml.etree.ElementTree as ET


def strip_tag_namespace(tag: str) -> str:
    """Removes XML namespace prefix like '{http://...}' from a tag name."""
    if "}" in tag:
        return tag.split("}", 1)[1]
    return tag


def parse_s3_xml(xml_content: str, mode: str) -> None:
    """Parses S3 ListBucketResult XML and writes requested data to stdout.

    Args:
        xml_content: Raw XML response string from S3.
        mode: Extraction target ('--keys', '--is-truncated', '--next-token').
    """
    if not xml_content.strip():
        return

    try:
        root = ET.fromstring(xml_content)
    except ET.ParseError as err:
        sys.stderr.write(f"XML parse error: {err}\n")
        return

    if mode == "--keys":
        for elem in root.iter():
            if strip_tag_namespace(elem.tag) == "Key" and elem.text:
                sys.stdout.write(f"{elem.text.strip()}\n")
    elif mode == "--is-truncated":
        for elem in root.iter():
            if strip_tag_namespace(elem.tag) == "IsTruncated" and elem.text:
                sys.stdout.write(f"{elem.text.strip().lower()}\n")
                return
        sys.stdout.write("false\n")
    elif mode == "--next-token":
        for elem in root.iter():
            tag_name = strip_tag_namespace(elem.tag)
            if tag_name in ("NextContinuationToken", "NextMarker") and elem.text:
                sys.stdout.write(f"{elem.text.strip()}\n")
                return


def main() -> None:
    if len(sys.argv) < 2:
        sys.stderr.write("Usage: parse-s3-xml.py <--keys|--is-truncated|--next-token>\n")
        sys.exit(1)

    mode = sys.argv[1]
    xml_data = sys.stdin.read()
    parse_s3_xml(xml_data, mode)


if __name__ == "__main__":
    main()
