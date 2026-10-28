#!/usr/bin/env bash
# Cross-builds OpenSSH's ssh and ssh-keygen for Android, which the app ships as
# lib*.so files: Oasis reaches saved machines by running `ssh`, and a phone has none.
# Built without OpenSSL or zlib (ed25519 keys, chacha20/aes-ctr with OpenSSH's own
# crypto). Patched to read ~ from $HOME, since an app's passwd entry says /data.
#
#   android/openssh/build.sh <aarch64|x86_64> <out-dir>
set -euo pipefail

VERSION=10.6p1
ARCH=${1:?arch: aarch64 or x86_64}
OUT=${2:?output directory}
API=29
NDK=${ANDROID_NDK_HOME:-$HOME/Android/Sdk/ndk/28.2.13676358}
TOOLS=$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin
CACHE=${OPENSSH_CACHE:-$HOME/.cache/oasis-android/openssh}
HOST=$ARCH-linux-android

mkdir -p "$CACHE" "$OUT"
TARBALL=$CACHE/openssh-$VERSION.tar.gz
if [ ! -f "$TARBALL" ]; then
    curl -sSfL -o "$TARBALL" "https://cdn.openbsd.org/pub/OpenBSD/OpenSSH/portable/openssh-$VERSION.tar.gz"
fi

SRC=$CACHE/build-$ARCH
if [ ! -f "$SRC/configured" ]; then
    rm -rf "$SRC"
    mkdir -p "$SRC"
    tar xzf "$TARBALL" -C "$SRC" --strip-components=1
    # ~ is $HOME: the app's passwd entry points at /data, which it cannot write.
    sed -i 's|^\tpw = pwcopy(pw);|\tpw = pwcopy(pw);\n\tif (getenv("HOME") != NULL \&\& *getenv("HOME") != '"'"'\\0'"'"') {\n\t\tfree(pw->pw_dir);\n\t\tpw->pw_dir = xstrdup(getenv("HOME"));\n\t}|' "$SRC/ssh.c"
    grep -q 'pw->pw_dir = xstrdup(getenv("HOME"))' "$SRC/ssh.c"
    # The same for ~ in paths (known_hosts, keys, config).
    python3 - "$SRC/misc.c" <<'PY'
import sys
path = sys.argv[1]
s = open(path).read()
anchor = "\t/* Make sure directory has a trailing '/' */\n"
assert anchor in s
s = s.replace(anchor, "\tconst char *home = pw->pw_dir;\n"
    "\tif (user == NULL && getenv(\"HOME\") != NULL && *getenv(\"HOME\") != '\\0')\n"
    "\t\thome = getenv(\"HOME\");\n\n" + anchor, 1)
old = ["(len = strlen(pw->pw_dir)) == 0 || pw->pw_dir[len - 1] != '/'", '"%s%s%s", pw->pw_dir,']
new = ["(len = strlen(home)) == 0 || home[len - 1] != '/'", '"%s%s%s", home,']
for o, n in zip(old, new):
    assert s.count(o) == 1, o
    s = s.replace(o, n)
open(path, "w").write(s)
PY
    # Bionic has no crypt(3); only sshd's password check uses it.
    sed -i 's|^#include "includes.h"$|#include "includes.h"\n#ifdef __ANDROID__\n#define crypt(key, salt) ((void)(key), (void)(salt), (char *)NULL)\n#endif|' "$SRC/openbsd-compat/xcrypt.c"
    # Bionic's bzero is a macro over an inline function; take that function's address.
    sed -i 's|= bzero;|= __bionic_bzero;|' "$SRC/openbsd-compat/explicit_bzero.c"
    # Bionic has no resolver state to look up SSHFP host keys in DNS; answer "not found".
    cat > "$SRC/openbsd-compat/getrrsetbyname.c" <<'STUB'
#include "includes.h"
#if !defined(HAVE_GETRRSETBYNAME) && !defined(HAVE_LDNS)
#include "getrrsetbyname.h"
int
getrrsetbyname(const char *hostname, unsigned int rdclass, unsigned int rdtype,
    unsigned int flags, struct rrsetinfo **res)
{
	return ERRSET_FAIL;
}
void
freerrset(struct rrsetinfo *rrset)
{
}
#endif
STUB
    (
        cd "$SRC"
        CC="$TOOLS/$HOST$API-clang" AR="$TOOLS/llvm-ar" RANLIB="$TOOLS/llvm-ranlib" \
            CFLAGS="-O2 -fPIE -DHAVE_ATTRIBUTE__SENTINEL__ -include strings.h" LDFLAGS="-pie" \
            ac_cv_func_bzero=yes ./configure --host="$HOST" --without-openssl --without-zlib \
            --without-pam --without-selinux --without-shadow --disable-strip \
            --disable-lastlog --disable-utmp --disable-utmpx --disable-wtmp --disable-wtmpx \
            --with-privsep-path=/nonexistent --with-xauth=/nonexistent \
            > configure.log 2>&1 || { tail -30 configure.log; exit 1; }
    )
    touch "$SRC/configured"
fi
make -C "$SRC" -j"$(nproc)" ssh ssh-keygen > "$SRC/make.log" 2>&1 || { tail -40 "$SRC/make.log"; exit 1; }
"$TOOLS/llvm-strip" -o "$OUT/libssh.so" "$SRC/ssh"
"$TOOLS/llvm-strip" -o "$OUT/libssh-keygen.so" "$SRC/ssh-keygen"
ls -l "$OUT/libssh.so" "$OUT/libssh-keygen.so"
