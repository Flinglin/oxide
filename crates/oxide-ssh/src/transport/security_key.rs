

struct SecurityKeySigner {
    key: Arc<PrivateKey>,
    provider: Arc<oxide_security_key::SecurityKeyProvider>,
    interaction: Arc<dyn oxide_security_key::SecurityKeyInteraction>,
}

fn verify_security_key_signature(
    key: &PrivateKey,
    required_flags: u8,
    mut to_sign: Vec<u8>,
    response: oxide_security_key::SecurityKeyResponse,
) -> Result<Vec<u8>, oxide_security_key::SecurityKeyError> {
    use oxide_security_key::{SecurityKeyError, SecurityKeyResponse};
    use signature::Verifier;
    let SecurityKeyResponse::Signature {
        signature,
        flags,
        counter,
    } = response
    else {
        return Err(SecurityKeyError::Incompatible);
    };
    if flags & required_flags & 0x05 != required_flags & 0x05 {
        return Err(SecurityKeyError::InvalidSignature);
    }
    let mut verification_bytes = signature.clone();
    verification_bytes.push(flags);
    verification_bytes.extend(counter.to_be_bytes());
    let verification = russh::keys::ssh_key::Signature::new(key.algorithm(), verification_bytes)
        .map_err(|_| SecurityKeyError::InvalidSignature)?;
    Verifier::verify(key.public_key(), &to_sign, &verification)
        .map_err(|_| SecurityKeyError::InvalidSignature)?;
    // Both SK algorithms put flags and counter after the signature
    // string; encode that wire contract independently of key-library helpers.
    let mut wire = Vec::new();
    key.algorithm()
        .encode(&mut wire)
        .map_err(|_| SecurityKeyError::InvalidSignature)?;
    signature
        .encode(&mut wire)
        .map_err(|_| SecurityKeyError::InvalidSignature)?;
    flags
        .encode(&mut wire)
        .map_err(|_| SecurityKeyError::InvalidSignature)?;
    counter
        .encode(&mut wire)
        .map_err(|_| SecurityKeyError::InvalidSignature)?;
    wire.encode(&mut to_sign)
        .map_err(|_| SecurityKeyError::InvalidSignature)?;
    Ok(to_sign)
}

impl SecurityKeySigner {
    async fn new(
        key: Arc<PrivateKey>,
        handler: Option<&dyn SshPromptHandler>,
    ) -> Result<Self, SshTransportError> {
        let handler = handler.ok_or_else(|| {
            SshTransportError::AuthenticationFailed(
                "Security-key signing requires a prompt handler".into(),
            )
        })?;
        let provider = handler
            .security_key_provider()
            .await
            .map_err(SshTransportError::AuthenticationFailed)?;
        let interaction = handler.security_key_interaction().ok_or_else(|| {
            SshTransportError::AuthenticationFailed(
                "Security-key interaction is unavailable".into(),
            )
        })?;
        Ok(Self {
            key,
            provider,
            interaction,
        })
    }
}

impl RusshSigner for SecurityKeySigner {
    type Error = LocalSignerError;

    fn auth_sign(
        &mut self,
        _key: &AgentIdentity,
        _hash_alg: Option<HashAlg>,
        to_sign: Vec<u8>,
    ) -> impl Future<Output = Result<Vec<u8>, Self::Error>> + Send {
        async move {
            use oxide_security_key::SecurityKeyRequest;
            let (application, handle, required_flags) = match self.key.key_data() {
                KeypairData::SkEd25519(key) => {
                    (key.public().application(), key.key_handle(), key.flags())
                }
                KeypairData::SkEcdsaSha2NistP256(key) => {
                    (key.public().application(), key.key_handle(), key.flags())
                }
                _ => {
                    return Err(LocalSignerError::Sign(
                        "Unsupported security-key algorithm".into(),
                    ));
                }
            };
            let request = SecurityKeyRequest::Sign {
                algorithm: self.key.algorithm().to_string(),
                application: application.to_string(),
                key_handle: Zeroizing::new(handle.to_vec()),
                flags: required_flags,
                challenge: to_sign.clone(),
            };
            let response = self
                .provider
                .sign(request, Arc::clone(&self.interaction))
                .await;
            let result = response.and_then(|response| {
                verify_security_key_signature(&self.key, required_flags, to_sign, response)
            });
            match result {
                Ok(signed) => Ok(signed),
                Err(error) => Err(LocalSignerError::Sign(
                    self.interaction.error_message(error).await,
                )),
            }
        }
    }
}
