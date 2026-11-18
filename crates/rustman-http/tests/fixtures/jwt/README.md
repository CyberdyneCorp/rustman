Throwaway key pairs for the JWT signing tests (RS256 and ES256), generated with:

    openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out rsa.pem
    openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out ec.pem
    openssl pkey -in <key>.pem -pubout -out <key>.pub.pem

They protect nothing; never reuse them.
