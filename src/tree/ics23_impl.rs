use anyhow::Result;

use crate::{
    proof::{SparseMerkleProof, INTERNAL_DOMAIN_SEPARATOR, LEAF_DOMAIN_SEPARATOR},
    storage::HasPreimage,
    storage::TreeReader,
    tree::ExclusionProof,
    JellyfishMerkleTree, KeyHash, Version, SPARSE_MERKLE_PLACEHOLDER_HASH,
};

fn sparse_merkle_proof_to_ics23_existence_proof(
    key: Vec<u8>,
    value: Vec<u8>,
    proof: &SparseMerkleProof,
) -> ics23::ExistenceProof {
    let key_hash: KeyHash = key.as_slice().into();
    let mut path = Vec::new();
    let mut skip = 256 - proof.siblings().len();
    let mut sibling_idx = 0;

    for byte_idx in (0..32).rev() {
        // The JMT proofs iterate over the bits in MSB order
        for bit_idx in 0..8 {
            if skip > 0 {
                skip -= 1;
                continue;
            } else {
                let bit = (key_hash.0[byte_idx] >> bit_idx) & 0x1;
                // ICS23 InnerOp computes
                //    hash( prefix || current || suffix )
                // so we want to construct (prefix, suffix) so that this is
                // the correct hash-of-internal-node
                let (prefix, suffix) = if bit == 1 {
                    // We want hash( domsep || sibling || current )
                    // so prefix = domsep || sibling
                    //    suffix = (empty)
                    let mut prefix = Vec::with_capacity(16 + 32);
                    prefix.extend_from_slice(INTERNAL_DOMAIN_SEPARATOR);
                    prefix.extend_from_slice(&proof.siblings()[sibling_idx]);
                    (prefix, Vec::new())
                } else {
                    // We want hash( domsep || current || sibling )
                    // so prefix = domsep
                    //    suffix = sibling
                    let prefix = INTERNAL_DOMAIN_SEPARATOR.to_vec();
                    let suffix = proof.siblings()[sibling_idx].to_vec();
                    (prefix, suffix)
                };
                path.push(ics23::InnerOp {
                    hash: ics23::HashOp::Sha256.into(),
                    prefix,
                    suffix,
                });
                sibling_idx += 1;
            }
        }
    }

    ics23::ExistenceProof {
        key: key_hash.0.to_vec(),
        value,
        path,
        leaf: Some(ics23::LeafOp {
            hash: ics23::HashOp::Sha256.into(),
            prehash_key: ics23::HashOp::NoHash.into(),
            prehash_value: ics23::HashOp::Sha256.into(),
            length: ics23::LengthOp::NoPrefix.into(),
            prefix: LEAF_DOMAIN_SEPARATOR.to_vec(),
        }),
    }
}

impl<'a, R> JellyfishMerkleTree<'a, R>
where
    R: 'a + TreeReader + HasPreimage,
{
    fn exclusion_proof_to_ics23_nonexistence_proof(
        &self,
        key: Vec<u8>,
        version: Version,
        proof: &ExclusionProof,
    ) -> Result<ics23::NonExistenceProof> {
        match proof {
            ExclusionProof::Leftmost {
                leftmost_right_proof,
            } => {
                let key_hash = leftmost_right_proof
                    .leaf()
                    .expect("must have leaf")
                    .key_hash();
                let key_left_proof = self
                    .reader
                    .preimage(key_hash)?
                    .ok_or(anyhow::anyhow!("missing preimage for key hash"))?;

                let value = self
                    .get(key_hash, version)?
                    .ok_or(anyhow::anyhow!("missing value for key hash"))?;

                let leftmost_right_proof = sparse_merkle_proof_to_ics23_existence_proof(
                    key_left_proof.clone(),
                    value.clone(),
                    leftmost_right_proof,
                );

                Ok(ics23::NonExistenceProof {
                    key: key_hash.0.to_vec(),
                    right: Some(leftmost_right_proof),
                    left: None,
                })
            }
            ExclusionProof::Middle {
                leftmost_right_proof,
                rightmost_left_proof,
            } => {
                let leftmost_key_hash = leftmost_right_proof
                    .leaf()
                    .expect("must have leaf")
                    .key_hash();
                let value_leftmost = self
                    .get(leftmost_key_hash, version)?
                    .ok_or(anyhow::anyhow!("missing value for key hash"))?;
                let key_leftmost = self
                    .reader
                    .preimage(leftmost_key_hash)?
                    .ok_or(anyhow::anyhow!("missing preimage for key hash"))?;
                let leftmost_right_proof = sparse_merkle_proof_to_ics23_existence_proof(
                    key_leftmost.clone(),
                    value_leftmost.clone(),
                    leftmost_right_proof,
                );

                let rightmost_key_hash = rightmost_left_proof
                    .leaf()
                    .expect("must have leaf")
                    .key_hash();
                let value_rightmost = self
                    .get(rightmost_key_hash, version)?
                    .ok_or(anyhow::anyhow!("missing value for key hash"))?;
                let key_rightmost = self
                    .reader
                    .preimage(rightmost_key_hash)?
                    .ok_or(anyhow::anyhow!("missing preimage for key hash"))?;
                let rightmost_left_proof = sparse_merkle_proof_to_ics23_existence_proof(
                    key_rightmost.clone(),
                    value_rightmost.clone(),
                    rightmost_left_proof,
                );

                Ok(ics23::NonExistenceProof {
                    key,
                    right: Some(leftmost_right_proof),
                    left: Some(rightmost_left_proof),
                })
            }
            ExclusionProof::Rightmost {
                rightmost_left_proof,
            } => {
                let rightmost_key_hash = rightmost_left_proof
                    .leaf()
                    .expect("must have leaf")
                    .key_hash();
                let value_rightmost = self
                    .get(rightmost_key_hash, version)?
                    .ok_or(anyhow::anyhow!("missing value for key hash"))?;
                let key_rightmost = self
                    .reader
                    .preimage(rightmost_key_hash)?
                    .ok_or(anyhow::anyhow!("missing preimage for key hash"))?;
                let rightmost_left_proof = sparse_merkle_proof_to_ics23_existence_proof(
                    key_rightmost.clone(),
                    value_rightmost.clone(),
                    rightmost_left_proof,
                );

                Ok(ics23::NonExistenceProof {
                    key,
                    right: None,
                    left: Some(rightmost_left_proof),
                })
            }
        }
    }

    /// Returns the value and an [`JMTProof`].
    pub fn get_with_ics23_proof(
        &self,
        key: Vec<u8>,
        version: Version,
    ) -> Result<ics23::CommitmentProof> {
        let key_hash = key.as_slice().into();
        let proof_or_exclusion = self.get_with_exclusion_proof(key_hash, version)?;

        match proof_or_exclusion {
            Ok((value, proof)) => {
                let ics23_exist =
                    sparse_merkle_proof_to_ics23_existence_proof(key, value.clone(), &proof);

                Ok(ics23::CommitmentProof {
                    proof: Some(ics23::commitment_proof::Proof::Exist(ics23_exist)),
                })
            }
            Err(exclusion_proof) => {
                let ics23_nonexist = self.exclusion_proof_to_ics23_nonexistence_proof(
                    key,
                    version,
                    &exclusion_proof,
                )?;

                Ok(ics23::CommitmentProof {
                    proof: Some(ics23::commitment_proof::Proof::Nonexist(ics23_nonexist)),
                })
            }
        }
    }
}

pub fn ics23_spec() -> ics23::ProofSpec {
    ics23::ProofSpec {
        prehash_compared_key: ics23::HashOp::Sha256.into(),
        prehash_compared_value: ics23::HashOp::NoHash.into(),
        leaf_spec: Some(ics23::LeafOp {
            hash: ics23::HashOp::Sha256.into(),
            prehash_key: 0,
            prehash_value: ics23::HashOp::Sha256.into(),
            length: ics23::LengthOp::NoPrefix.into(),
            prefix: LEAF_DOMAIN_SEPARATOR.to_vec(),
        }),
        inner_spec: Some(ics23::InnerSpec {
            hash: ics23::HashOp::Sha256.into(),
            child_order: vec![0, 1],
            min_prefix_length: INTERNAL_DOMAIN_SEPARATOR.len() as i32,
            max_prefix_length: INTERNAL_DOMAIN_SEPARATOR.len() as i32,
            child_size: 32,
            empty_child: SPARSE_MERKLE_PLACEHOLDER_HASH.to_vec(),
        }),
        min_depth: 0,
        max_depth: 64,
    }
}

#[cfg(test)]
mod tests {
    use ics23::HostFunctionsManager;
    use proptest::prelude::*;

    use super::*;
    use crate::{mock::MockTreeStore, KeyHash, SPARSE_MERKLE_PLACEHOLDER_HASH};

    proptest! {
        #[test]
        fn test_jmt_ics23_nonexistence(
            keys: Vec<Vec<u8>>,
        ) {
            let db = MockTreeStore::default();
            let tree = JellyfishMerkleTree::new(&db);

            let mut kvs = Vec::new();

            kvs.push((KeyHash::from(b"key"), Some(b"value1".to_vec())));
            db.put_key_preimage(&b"key".to_vec());
            for key_preimage in keys {
                let key_hash = KeyHash::from(&key_preimage);
                let value = vec![0u8; 32];
                kvs.push((key_hash, Some(value)));
                db.put_key_preimage(&key_preimage.to_vec());
            }

            let (new_root_hash, batch) = tree.put_value_set(kvs, 0).unwrap();
            db.write_tree_update_batch(batch).unwrap();

            let commitment_proof = tree
                .get_with_ics23_proof(b"notexist".to_vec(), 0)
                .unwrap();


            let key_hash = b"notexist".as_slice().into();
            let proof_or_exclusion = tree.get_with_exclusion_proof(key_hash, 0).unwrap();

            use crate::tree::ExclusionProof::{Leftmost, Rightmost, Middle};
            match proof_or_exclusion {
                Ok(_) => panic!("expected nonexistence proof"),
                Err(exclusion_proof) => {
                    match exclusion_proof {
                        Leftmost { leftmost_right_proof } => {
                            if leftmost_right_proof.root_hash() != new_root_hash {
                                panic!("root hash mismatch. siblings: {:?}, smph: {:?}", leftmost_right_proof.siblings(), SPARSE_MERKLE_PLACEHOLDER_HASH);
                            }

                            assert!(ics23::verify_non_membership::<HostFunctionsManager>(
                                &commitment_proof,
                                &ics23_spec(),
                                &new_root_hash.0.to_vec(),
                                b"notexist"
                            ))
                        },
                        Rightmost { rightmost_left_proof } => {
                            if rightmost_left_proof.root_hash() != new_root_hash {
                                panic!("root hash mismatch. siblings: {:?}, smph: {:?}", rightmost_left_proof.siblings(), SPARSE_MERKLE_PLACEHOLDER_HASH);
                            }

                            assert!(ics23::verify_non_membership::<HostFunctionsManager>(
                                &commitment_proof,
                                &ics23_spec(),
                                &new_root_hash.0.to_vec(),
                                b"notexist"
                            ))
                        },
                        Middle {
                            leftmost_right_proof,
                            rightmost_left_proof,
                        } => {
                            if leftmost_right_proof.root_hash() != new_root_hash {
                                let good_proof = tree.get_with_proof(leftmost_right_proof.leaf().unwrap().key_hash(), 0).unwrap();
                                panic!("root hash mismatch. bad proof: {:?}, good proof: {:?}", leftmost_right_proof, good_proof);
                            }
                            if rightmost_left_proof.root_hash() != new_root_hash {
                                panic!("root hash mismatch. siblings: {:?}", rightmost_left_proof.siblings());
                            }

                            assert!(ics23::verify_non_membership::<HostFunctionsManager>(
                                &commitment_proof,
                                &ics23_spec(),
                                &new_root_hash.0.to_vec(),
                                b"notexist"
                            ))

                        },
                    }
                }
            }

            assert!(!ics23::verify_non_membership::<HostFunctionsManager>(
                &commitment_proof,
                &ics23_spec(),
                &new_root_hash.0.to_vec(),
                b"key",
            ));
        }
    }

    #[test]
    fn test_jmt_ics23_existence() {
        let db = MockTreeStore::default();
        let tree = JellyfishMerkleTree::new(&db);

        let key = b"key";
        let key_hash = KeyHash::from(&key);

        // For testing, insert multiple values into the tree
        let mut kvs = Vec::new();
        kvs.push((key_hash, Some(b"value".to_vec())));
        // make sure we have some sibling nodes, through carefully constructed k/v entries that will have overlapping paths
        for i in 1..4 {
            let mut overlap_key = KeyHash([0; 32]);
            overlap_key.0[0..i].copy_from_slice(&key_hash.0[0..i]);
            kvs.push((overlap_key, Some(b"bogus value".to_vec())));
        }

        let (new_root_hash, batch) = tree.put_value_set(kvs, 0).unwrap();
        db.write_tree_update_batch(batch).unwrap();

        let commitment_proof = tree.get_with_ics23_proof(b"key".to_vec(), 0).unwrap();

        assert!(ics23::verify_membership::<HostFunctionsManager>(
            &commitment_proof,
            &ics23_spec(),
            &new_root_hash.0.to_vec(),
            b"key",
            b"value",
        ));
    }

    #[test]
    fn test_jmt_ics23_existence_random_keys() {
        let db = MockTreeStore::default();
        let tree = JellyfishMerkleTree::new(&db);

        const MAX_VERSION: u64 = 1 << 14;

        for version in 0..=MAX_VERSION {
            let key = format!("key{}", version).into_bytes();
            let value = format!("value{}", version).into_bytes();
            let (_root, batch) = tree
                .put_value_set(vec![(key.as_slice().into(), Some(value))], version)
                .unwrap();
            db.write_tree_update_batch(batch).unwrap();
        }

        let commitment_proof = tree
            .get_with_ics23_proof(format!("key{}", MAX_VERSION).into_bytes(), MAX_VERSION)
            .unwrap();

        let root_hash = tree.get_root_hash(MAX_VERSION).unwrap().0.to_vec();

        assert!(ics23::verify_membership::<HostFunctionsManager>(
            &commitment_proof,
            &ics23_spec(),
            &root_hash,
            format!("key{}", MAX_VERSION).as_bytes(),
            format!("value{}", MAX_VERSION).as_bytes(),
        ));
    }
}
