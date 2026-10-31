import base64, json, subprocess, sys, re, os
d = sys.argv[1]
def b64u(b): return base64.urlsafe_b64encode(b).rstrip(b'=').decode()
def run(*a, inp=None):
    return subprocess.run(a, input=inp, capture_output=True, check=True).stdout
def keypair(kind, name):
    if kind == 'rsa':
        run('openssl','genrsa','-out',f'{d}/{name}.pem','2048')
    else:
        run('openssl','ecparam','-name','prime256v1','-genkey','-noout','-out',f'{d}/{name}.pem')
    return f'{d}/{name}.pem'
def rsa_jwk(pem, kid):
    out = run('openssl','rsa','-in',pem,'-noout','-modulus').decode().strip()
    n = bytes.fromhex(out.split('=')[1])
    text = run('openssl','rsa','-in',pem,'-noout','-text').decode()
    e = int(re.search(r'publicExponent: (\d+)', text).group(1))
    eb = e.to_bytes((e.bit_length()+7)//8,'big')
    return {"kty":"RSA","kid":kid,"alg":"RS256","n":b64u(n),"e":b64u(eb)}
def ec_jwk(pem, kid):
    out = run('openssl','ec','-in',pem,'-pubout','-outform','DER')
    pt = out[-65:]
    assert pt[0]==4
    return {"kty":"EC","kid":kid,"alg":"ES256","crv":"P-256","x":b64u(pt[1:33]),"y":b64u(pt[33:])}
def der_to_raw(der):
    assert der[0]==0x30
    i=2 if der[1]<0x80 else 3
    def rd(i):
        assert der[i]==2
        l=der[i+1]; v=der[i+2:i+2+l]; return int.from_bytes(v,'big'), i+2+l
    r,i=rd(i); s,i=rd(i)
    return r.to_bytes(32,'big')+s.to_bytes(32,'big')
def sign(alg, pem, header, payload):
    si = b64u(json.dumps(header,separators=(',',':')).encode())+'.'+b64u(json.dumps(payload,separators=(',',':')).encode())
    sig = run('openssl','dgst','-sha256','-sign',pem,inp=si.encode())
    if alg=='ES256': sig=der_to_raw(sig)
    return si+'.'+b64u(sig)
rs = keypair('rsa','rs256-key'); es = keypair('ec','es256-key'); other = keypair('rsa','rs256-other-key')
keys = [rsa_jwk(rs,'rs-1'), ec_jwk(es,'es-1')]
other_jwk = rsa_jwk(other,'rs-1')
base = {"iss":"https://issuer.test","aud":"app-1","sub":"user-1","iat":1700000000,"nbf":1700000000,"exp":2000000000,"email":"u@example.com","roles":["a","b"]}
def tok(alg, pem, kid, **over):
    p = dict(base); 
    for k,v in over.items():
        if v is None: p.pop(k,None)
        else: p[k]=v
    h = {"alg":alg,"typ":"JWT"}
    if kid: h["kid"]=kid
    return sign(alg, pem, h, p)
tokens = {
 "rs256_valid": tok('RS256', rs, 'rs-1'),
 "es256_valid": tok('ES256', es, 'es-1'),
 "rs256_no_kid": tok('RS256', rs, None),
 "rs256_expired": tok('RS256', rs, 'rs-1', exp=1700000100),
 "rs256_not_yet_valid": tok('RS256', rs, 'rs-1', nbf=1900000000),
 "rs256_wrong_issuer": tok('RS256', rs, 'rs-1', iss="https://evil.test"),
 "rs256_wrong_audience": tok('RS256', rs, 'rs-1', aud="app-2"),
 "rs256_audience_list": tok('RS256', rs, 'rs-1', aud=["x","app-1"]),
 "rs256_unknown_kid": tok('RS256', rs, 'nope'),
 "rs256_no_subject": tok('RS256', rs, 'rs-1', sub=None),
 "rs256_no_exp": tok('RS256', rs, 'rs-1', exp=None),
 "rs256_signed_by_other_key": tok('RS256', other, 'rs-1'),
}
json.dump({"keys":keys,"otherKeys":[other_jwk],"claims":base,"tokens":tokens}, open(f'{d}/jws-verify.json','w'), indent=2)
