#!/usr/bin/env bash
# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
#
# Does the setup .rpm install on the systems it is for? One container per system, nothing is kept:
#   - fedora:latest and the Fedora release before it (the number is read from the latest image):
#     dnf -y install ./x.rpm. Fedora 45 checks signatures by default, but dnf5 leaves a local file
#     alone (localpkg_gpgcheck=False): https://fedoraproject.org/wiki/Changes/Enforcing_signature_checking_by_default
#     and https://github.com/rpm-software-management/dnf5/issues/2614 (it was broken until fixed).
#   - opensuse/tumbleweed: zypper --non-interactive install --allow-unsigned-rpm ./x.rpm. The option
#     belongs to the install command: in front of it zypper 1.14.102 answers "The flag
#     --allow-unsigned-rpm is not known." and exits 2
#     (https://manpages.opensuse.org/Tumbleweed/zypper/zypper.8.en.html, section install)
# Then, in each: rpm -qpR lists /usr/bin/pkexec and at least one soname, and ldd on the installed
# program finds every library. These are the checks that decide whether the file is published.
# Informational only, never failing the script: almalinux:10 with EPEL (webkit2gtk4.1 is only in EPEL
# 10 testing, https://mdapi.fedoraproject.org/epel10/pkg/webkit2gtk4.1) to see whether the Red Hat
# family can be named as supported later.
#
# Usage: tools/packaging/rpm_install_check.sh path/to/printpapi-setup.rpm      (needs docker)
set -u

rpm_path=${1:?usage: rpm_install_check.sh <file.rpm>}
[ -f "$rpm_path" ] || { echo "no such file: $rpm_path" >&2; exit 2; }
dir=$(cd "$(dirname "$rpm_path")" && pwd)
file=$(basename "$rpm_path")

# what runs inside a container: the install command comes in as INSTALL, the checks follow
inner='
set -eu
eval "$INSTALL \"/pkg/$FILE\""
echo "--- requires of the package"
rpm -qpR "/pkg/$FILE" | tee /tmp/requires
grep -q "^/usr/bin/pkexec" /tmp/requires || { echo "missing: /usr/bin/pkexec in the requires"; exit 1; }
grep -Eq "\.so" /tmp/requires || { echo "missing: no soname in the requires"; exit 1; }
echo "--- libraries of /usr/bin/printpapi-setup"
ldd /usr/bin/printpapi-setup
if ldd /usr/bin/printpapi-setup | grep "not found"; then echo "missing libraries"; exit 1; fi
'

run_in() { # image install-command
  docker run --rm -v "$dir:/pkg:ro" -e "FILE=$file" -e "INSTALL=$2" "$1" bash -c "$inner"
}

failed=0
check() { # image install-command
  echo "::group::$1"
  if run_in "$1" "$2"; then echo "ok: $1"; else echo "::error::the .rpm does not install or check out in $1"; failed=1; fi
  echo "::endgroup::"
}

latest=$(docker run --rm fedora:latest bash -c '. /etc/os-release; echo "$VERSION_ID"') || latest=""
check fedora:latest "dnf -y install"
if [[ "$latest" =~ ^[0-9]+$ ]] && [ "$latest" -gt 1 ]; then
  check "fedora:$((latest - 1))" "dnf -y install"
else
  echo "::warning::could not read the number of the latest Fedora; the previous release is not checked"
fi
check opensuse/tumbleweed "zypper --non-interactive --gpg-auto-import-keys install --allow-unsigned-rpm"

echo "::group::almalinux:10 with EPEL (informational)"
if run_in almalinux:10 "dnf -y install epel-release && dnf -y --enablerepo=epel-testing install"; then
  echo "info: it installs on AlmaLinux 10 with EPEL"
else
  echo "info: it does not install on AlmaLinux 10 with EPEL (the Red Hat family stays unsupported)"
fi
echo "::endgroup::"

exit "$failed"
