These inert archives contain `owned-fixture-binary\n` as `photonic` (tar.gz) or
`photonic.exe` (ZIP), plus unrelated traversal and symlink entries. No executable
is installed or run. Signed fixtures use zipsign-api 0.2.2 and the filename as
signature context. The public fixture key is derived from the deliberately
public synthetic seed `[42; 32]`; it has no release authority. Unsigned fixtures
exercise mandatory signature rejection. All extraction occurs in a unique
temporary directory. ZIP9 advisory regression fixtures are exercised through
the updater's named regular-file extraction boundary.
