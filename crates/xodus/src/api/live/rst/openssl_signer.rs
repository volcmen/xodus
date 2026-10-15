//! RSA-SHA256 signing through OpenSSL, never RustCrypto private-key operations.

use kryptering::{HashAlgorithm, SignatureAlgorithm, traits::Signer};
use openssl::{
    hash::MessageDigest,
    pkey::{PKey, Private},
    rsa::Padding,
};
use rsa::pkcs8::EncodePrivateKey;

pub(in crate::api::live) struct OpenSslRsaSigner(PKey<Private>);

impl OpenSslRsaSigner {
    pub(in crate::api::live) fn new(key: &rsa::RsaPrivateKey) -> Result<Self, bergshamra::Error> {
        let der = key
            .to_pkcs8_der()
            .map_err(|_| bergshamra::Error::Key("RSA key encoding failed".into()))?;
        let key = PKey::private_key_from_der(der.as_bytes())
            .map_err(|_| bergshamra::Error::Key("RSA key import failed".into()))?;
        // SecretDocument zeroizes the temporary DER when it goes out of scope.
        Ok(Self(key))
    }
}

impl Signer for OpenSslRsaSigner {
    fn algorithm(&self) -> SignatureAlgorithm {
        SignatureAlgorithm::RsaPkcs1v15(HashAlgorithm::Sha256)
    }

    fn sign(&self, data: &[u8]) -> kryptering::Result<Vec<u8>> {
        let sign = || -> Result<Vec<u8>, openssl::error::ErrorStack> {
            let mut signer = openssl::sign::Signer::new(MessageDigest::sha256(), &self.0)?;
            signer.set_rsa_padding(Padding::PKCS1)?;
            signer.update(data)?;
            signer.sign_to_vec()
        };
        sign().map_err(|_| kryptering::Error::Crypto("OpenSSL RSA signing failed".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::{pkcs8::DecodePrivateKey, signature::Verifier};

    #[test]
    fn signatures_verify_with_the_existing_public_key_and_reject_tampering() {
        let native = openssl::rsa::Rsa::generate(2048).unwrap();
        let native = PKey::from_rsa(native).unwrap();
        let der = native.private_key_to_pkcs8().unwrap();
        let key = rsa::RsaPrivateKey::from_pkcs8_der(&der).unwrap();
        let signer = OpenSslRsaSigner::new(&key).unwrap();
        let data = b"canonical SOAP SignedInfo fixture";
        let signature = signer.sign(data).unwrap();
        let verifier = rsa::pkcs1v15::VerifyingKey::<rsa::sha2::Sha256>::new(key.to_public_key());
        let signature = rsa::pkcs1v15::Signature::try_from(signature.as_slice()).unwrap();
        verifier.verify(data, &signature).unwrap();
        assert!(verifier.verify(b"changed SignedInfo", &signature).is_err());
        assert_eq!(
            signer.algorithm(),
            SignatureAlgorithm::RsaPkcs1v15(HashAlgorithm::Sha256)
        );
    }

    #[test]
    fn complete_soap_request_verifies_and_rejects_changed_scope() {
        use crate::api::live::rst::{RSTRequestBuilder, RSTSignature};
        let native = PKey::from_rsa(openssl::rsa::Rsa::generate(2048).unwrap()).unwrap();
        let key =
            rsa::RsaPrivateKey::from_pkcs8_der(&native.private_key_to_pkcs8().unwrap()).unwrap();
        let public = key.to_public_key();
        let request = RSTRequestBuilder::new()
            .scope_policy("https://example.invalid/original", None)
            .signature(RSTSignature::Rsa(Box::new(key)))
            .build()
            .unwrap();
        let mut keys = bergshamra::KeysManager::new();
        keys.add_key(bergshamra::Key::new(
            bergshamra::KeyData::Rsa {
                private: None,
                public,
            },
            bergshamra::KeyUsage::Verify,
        ));
        let context = bergshamra::DsigContext::new(keys).with_strict_verification(false);
        assert!(
            bergshamra::verify(&context, &request.signed_xml)
                .unwrap()
                .is_valid()
        );
        let changed = request.signed_xml.replace(
            "https://example.invalid/original",
            "https://example.invalid/tampered",
        );
        assert_ne!(changed, request.signed_xml);
        assert!(!bergshamra::verify(&context, &changed).unwrap().is_valid());
    }
}
