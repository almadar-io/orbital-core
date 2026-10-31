# security fixtures

`jws-verify.json` is the known-answer set for the `jws/verify` operator (EXEC-IDENTITY-2). Its keys are throwaway test keys
(`rs256-key.pem`, `rs256-other-key.pem`, `es256-key.pem`); nothing here protects anything.

Regenerate with `python3 -I gen-jws-verify.py <this directory>`. The script runs these openssl commands and signs offline:

```
openssl genrsa -out rs256-key.pem 2048
openssl genrsa -out rs256-other-key.pem 2048
openssl ecparam -name prime256v1 -genkey -noout -out es256-key.pem
openssl rsa -in rs256-key.pem -noout -modulus          # JWK n
openssl rsa -in rs256-key.pem -noout -text             # JWK e
openssl ec -in es256-key.pem -pubout -outform DER      # last 65 bytes: 04 || x || y (JWK x, y)
openssl dgst -sha256 -sign <key>.pem                   # over the ASCII header.payload; ES256 DER is converted to raw r||s
```

Claims are fixed (`nbf` 1700000000, `exp` 2000000000, `iss` https://issuer.test, `aud` app-1, `sub` user-1); a test pins the
dispatch clock inside that window. `jwt-bearer.json` is the OAuth jwt-bearer grant fixture.
