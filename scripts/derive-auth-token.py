#!/usr/bin/env python3
"""
Derive deterministic HMAC-SHA256 bearer token for PgVisor cluster authentication.
Usage: ./scripts/derive-auth-token.py <cluster_secret>
"""
import hashlib
import hmac
import sys

def main():
    if len(sys.argv) < 2:
        print("Usage: derive-auth-token.py <cluster_secret>", file=sys.stderr)
        sys.exit(1)
    secret = sys.argv[1].encode("utf-8")
    token = hmac.new(secret, b"pgvisor-internal-v1", hashlib.sha256).hexdigest()
    print(token)

if __name__ == "__main__":
    main()
