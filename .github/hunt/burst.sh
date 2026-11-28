#!/usr/bin/env bash
# Temporary: boots a fresh Keycloak ROUNDS times; each time signs in otto, lena, lena and piet at
# once by the code flow, as the first sign-ins of the caught failure did, then each of them again
# one at a time, and prints whether each ID token carries the groups claim.
# ARM: plain (on a hit, asks the admin API, then clears the user cache and then the realm cache,
# signing in again after each) or warm (one sign-in of lena alone before the burst).
set -u
rounds=${ROUNDS:-40}
arm=${ARM:-plain}
image=${IMAGE:-quay.io/keycloak/keycloak:26.8.2}
base=http://127.0.0.1:55080
kc=$base/realms/helpdesk/protocol/openid-connect
admin=$base/admin/realms/helpdesk
redirect=http://127.0.0.1:8080/api/session/identity-provider/callback
verifier=verifier-of-the-keycloak-group-burst-0123456789-abcdefghij
challenge=$(printf '%s' "$verifier" | openssl dgst -sha256 -binary | base64 | tr '+/' '-_' | tr -d '=')
work=$(mktemp -d)

sign_in() { # user tag → "user=groups" or "user=NONE" or "user=failed"
  local user=$1 jar=$work/$2.jar page action location code token payload
  page=$(curl -s -c "$jar" -b "$jar" "$kc/auth?client_id=hd-core&response_type=code&scope=openid&redirect_uri=$redirect&state=s$2&nonce=n$2&code_challenge=$challenge&code_challenge_method=S256")
  action=$(printf '%s' "$page" | grep -o 'id="kc-form-login"[^>]*action="[^"]*"' | sed 's/.*action="//; s/"$//; s/&amp;/\&/g')
  location=$(curl -s -o /dev/null -w '%{redirect_url}' -c "$jar" -b "$jar" \
    --data-urlencode "username=$user" --data-urlencode "password=$user-dev-password" \
    --data-urlencode "credentialId=" "$action")
  code=$(printf '%s' "$location" | sed -n 's/.*[?&]code=\([^&]*\).*/\1/p')
  token=$(curl -s -u hd-core:dev-only-client-secret -d grant_type=authorization_code \
    --data-urlencode "code=$code" --data-urlencode "redirect_uri=$redirect" \
    -d "code_verifier=$verifier" "$kc/token" | sed -n 's/.*"id_token":"\([^"]*\)".*/\1/p')
  payload=$(printf '%s' "$token" | cut -d. -f2 | tr '_-' '/+')
  while [ $(( ${#payload} % 4 )) -ne 0 ]; do payload="$payload="; done
  payload=$(printf '%s' "$payload" | base64 -d 2>/dev/null)
  if [ -z "$token" ]; then echo "$user=failed"
  elif printf '%s' "$payload" | grep -q '"groups"'; then echo "$user=groups"
  else echo "$user=NONE"; fi
}

diagnose() { # what the admin API says, and which cache clearing brings the groups back
  local at client
  at=$(curl -s -d grant_type=password -d client_id=admin-cli -d username=hunt \
    -d password=hunt-only-password "$base/realms/master/protocol/openid-connect/token" |
    sed -n 's/.*"access_token":"\([^"]*\)".*/\1/p')
  echo "  lena's groups by the admin API: $(curl -s -H "Authorization: Bearer $at" \
    "$admin/users/6f1c2a0e-8d3b-4c55-9a41-0b7e2d6f3a01/groups")"
  client=$(curl -s -H "Authorization: Bearer $at" "$admin/clients?clientId=hd-core" |
    sed -n 's/^\[{"id":"\([^"]*\)".*/\1/p')
  echo "  hd-core's mappers by the admin API: $(curl -s -H "Authorization: Bearer $at" \
    "$admin/clients/$client/protocol-mappers/models" | grep -o '"name":"[^"]*"' | tr '\n' ' ')"
  echo "  again before clearing: $(sign_in lena "$round-d0")"
  curl -s -o /dev/null -X POST -H "Authorization: Bearer $at" "$admin/clear-user-cache"
  echo "  after clearing the user cache: $(sign_in lena "$round-d1") $(sign_in piet "$round-d1p")"
  curl -s -o /dev/null -X POST -H "Authorization: Bearer $at" "$admin/clear-realm-cache"
  echo "  after clearing the realm cache: $(sign_in lena "$round-d2") $(sign_in piet "$round-d2p")"
}

for round in $(seq "$rounds"); do
  docker rm -f keycloak > /dev/null 2>&1 || true
  docker run -d --name keycloak -p 127.0.0.1:55080:8080 -e KC_HOSTNAME=$base \
    -e KC_BOOTSTRAP_ADMIN_USERNAME=hunt -e KC_BOOTSTRAP_ADMIN_PASSWORD=hunt-only-password \
    -v "$GITHUB_WORKSPACE/dev/keycloak:/opt/keycloak/data/import:ro" \
    "$image" start-dev --import-realm --log-level=info,org.keycloak.events:debug > /dev/null
  for _ in $(seq 90); do
    curl -sf "$base/realms/helpdesk/.well-known/openid-configuration" > /dev/null && break
    sleep 1
  done
  # As in the caught round: a wrong secret and an invalid code before the first sign-ins.
  curl -s -o /dev/null -u hd-core:wrong -d grant_type=authorization_code -d code=x "$kc/token"
  curl -s -o /dev/null -u hd-core:dev-only-client-secret -d grant_type=authorization_code -d code=x "$kc/token"
  warm=""
  [ "$arm" = warm ] && warm="warm: $(sign_in lena "$round-w") |"
  burst=$(i=0; for u in otto lena lena piet; do i=$((i + 1)); sign_in "$u" "$round-$u-$i" & done; wait)
  sleep 1
  after=$(for u in lena otto piet; do sign_in "$u" "$round-again-$u"; done)
  line="round $round $arm: $warm burst: $(echo $burst) | after: $(echo $after)"
  echo "$line"
  case "$line" in *NONE*|*failed*)
    mkdir -p "$RUNNER_TEMP/caught"
    echo "$line" > "$RUNNER_TEMP/caught/round-$round.txt"
    [ "$arm" = plain ] && diagnose | tee "$RUNNER_TEMP/caught/round-$round-diagnosis.txt"
    docker logs keycloak > "$RUNNER_TEMP/caught/round-$round-keycloak.log" 2>&1 ;;
  esac
done
