"""Validate every unsigned release artifact before the signing key is loaded."""

import hashlib
import stat
import tarfile
import zipfile
from pathlib import Path


ARCHIVES = {
    "photonic-x86_64-unknown-linux-gnu.tar.gz": ("tar", "photonic"),
    "photonic-x86_64-apple-darwin.tar.gz": ("tar", "photonic"),
    "photonic-aarch64-apple-darwin.tar.gz": ("tar", "photonic"),
    "photonic-x86_64-pc-windows-msvc.zip": ("zip", "photonic.exe"),
}
PUBLIC_KEY_SHA256 = "34642dd944fd2940db6f08843aa8a2c31e66e5df16901b6d9be52c8509b1243d"


def verify(root: Path, public_key: Path) -> None:
    expected = set(ARCHIVES) | {f"{name}.sha256" for name in ARCHIVES}
    actual = {path.relative_to(root).as_posix() for path in root.rglob("*")}
    if actual != expected:
        raise ValueError(f"unexpected artifact files: {sorted(actual ^ expected)}")

    key_bytes = public_key.read_bytes()
    if len(key_bytes) != 32 or hashlib.sha256(key_bytes).hexdigest() != PUBLIC_KEY_SHA256:
        raise ValueError("release public key does not match the reviewed key")

    for name, (kind, member_name) in ARCHIVES.items():
        archive = root / name
        checksum = root / f"{name}.sha256"
        if any(not path.is_file() or path.is_symlink() for path in (archive, checksum)):
            raise ValueError(f"{name}: archive and checksum must be regular files")

        parts = checksum.read_text(encoding="ascii").split()
        if len(parts) != 2 or parts[1] != name:
            raise ValueError(f"{name}: invalid checksum manifest")
        digest = parts[0].lower()
        if len(digest) != 64 or any(c not in "0123456789abcdef" for c in digest):
            raise ValueError(f"{name}: invalid SHA-256")
        sha = hashlib.sha256()
        with archive.open("rb") as handle:
            for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                sha.update(chunk)
        if sha.hexdigest() != digest:
            raise ValueError(f"{name}: checksum mismatch")

        if kind == "tar":
            with tarfile.open(archive, "r:gz") as container:
                members = container.getmembers()
                if (
                    len(members) != 1
                    or members[0].name != member_name
                    or not members[0].isfile()
                    or members[0].size <= 0
                ):
                    raise ValueError(f"{name}: unexpected tar member")
        else:
            with zipfile.ZipFile(archive) as container:
                members = container.infolist()
                if len(members) != 1:
                    raise ValueError(f"{name}: unexpected zip members")
                member = members[0]
                mode = (member.external_attr >> 16) & 0o170000
                if (
                    member.filename != member_name
                    or member.is_dir()
                    or mode not in (0, stat.S_IFREG)
                    or member.file_size <= 0
                ):
                    raise ValueError(f"{name}: unexpected zip member")
        print(f"verified {name} ({digest})")


if __name__ == "__main__":
    verify(Path("unsigned"), Path("release/photonic-signing.pub"))
