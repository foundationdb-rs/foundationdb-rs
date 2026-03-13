// Copyright 2024 foundationdb-rs developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

mod common;

#[cfg(feature = "recipes-ranked-register")]
mod ranked_register_tests {
    use foundationdb::{
        recipes::ranked_register::{Rank, RankedRegister, WriteResult},
        tuple::Subspace,
        Database, FdbBindingError,
    };

    #[test]
    fn test_ranked_register() {
        let _guard = unsafe { foundationdb::boot() };
        futures::executor::block_on(test_basic_write_and_read()).expect("failed to run");
        futures::executor::block_on(test_rank_fencing()).expect("failed to run");
        futures::executor::block_on(test_write_ordering()).expect("failed to run");
        futures::executor::block_on(test_stale_write_rejected()).expect("failed to run");
        futures::executor::block_on(test_follower_value_read()).expect("failed to run");
        futures::executor::block_on(test_empty_register()).expect("failed to run");
    }

    async fn setup_test(db: &Database, test_name: &str) -> Result<RankedRegister, FdbBindingError> {
        let subspace = Subspace::all().subspace(&test_name);
        let (from, to) = subspace.range();

        let from_ref = &from;
        let to_ref = &to;
        db.run(|txn, _| async move {
            txn.clear_range(from_ref, to_ref);
            Ok(())
        })
        .await?;

        Ok(RankedRegister::new(subspace))
    }

    async fn test_basic_write_and_read() -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let rr = setup_test(&db, "test_basic_write_and_read").await?;

        // Write with rank 1
        let rr_ref = &rr;
        let result = db
            .run(|txn, _| async move {
                let r = rr_ref
                    .write(&txn, Rank::from(1u64), b"hello")
                    .await
                    .map_err(|e| {
                        foundationdb::FdbBindingError::CustomError(e.to_string().into())
                    })?;
                Ok(r)
            })
            .await?;
        assert_eq!(result, WriteResult::Committed);

        // Read with rank 2 should return the value
        let rr_ref = &rr;
        let read_result = db
            .run(|txn, _| async move {
                let r = rr_ref.read(&txn, Rank::from(2u64)).await.map_err(|e| {
                    foundationdb::FdbBindingError::CustomError(e.to_string().into())
                })?;
                Ok((r.write_rank(), r.into_value()))
            })
            .await?;
        assert_eq!(read_result.0, Rank::from(1u64));
        assert_eq!(read_result.1.as_deref(), Some(b"hello".as_slice()));

        Ok(())
    }

    async fn test_rank_fencing() -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let rr = setup_test(&db, "test_rank_fencing").await?;

        // Read with rank 10 installs a fence
        let rr_ref = &rr;
        db.run(|txn, _| async move {
            rr_ref
                .read(&txn, Rank::from(10u64))
                .await
                .map_err(|e| foundationdb::FdbBindingError::CustomError(e.to_string().into()))?;
            Ok(())
        })
        .await?;

        // Write with rank 5 should be aborted (below the fence)
        let rr_ref = &rr;
        let result = db
            .run(|txn, _| async move {
                let r = rr_ref
                    .write(&txn, Rank::from(5u64), b"blocked")
                    .await
                    .map_err(|e| {
                        foundationdb::FdbBindingError::CustomError(e.to_string().into())
                    })?;
                Ok(r)
            })
            .await?;
        assert_eq!(result, WriteResult::Aborted);

        // Write with rank 10 should succeed (equal to fence)
        let rr_ref = &rr;
        let result = db
            .run(|txn, _| async move {
                let r = rr_ref
                    .write(&txn, Rank::from(10u64), b"accepted")
                    .await
                    .map_err(|e| {
                        foundationdb::FdbBindingError::CustomError(e.to_string().into())
                    })?;
                Ok(r)
            })
            .await?;
        assert_eq!(result, WriteResult::Committed);

        Ok(())
    }

    async fn test_write_ordering() -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let rr = setup_test(&db, "test_write_ordering").await?;

        // Write rank 1 value "A"
        let rr_ref = &rr;
        let result = db
            .run(|txn, _| async move {
                let r = rr_ref
                    .write(&txn, Rank::from(1u64), b"A")
                    .await
                    .map_err(|e| {
                        foundationdb::FdbBindingError::CustomError(e.to_string().into())
                    })?;
                Ok(r)
            })
            .await?;
        assert_eq!(result, WriteResult::Committed);

        // Write rank 2 value "B"
        let rr_ref = &rr;
        let result = db
            .run(|txn, _| async move {
                let r = rr_ref
                    .write(&txn, Rank::from(2u64), b"B")
                    .await
                    .map_err(|e| {
                        foundationdb::FdbBindingError::CustomError(e.to_string().into())
                    })?;
                Ok(r)
            })
            .await?;
        assert_eq!(result, WriteResult::Committed);

        // Read should return "B"
        let rr_ref = &rr;
        let read_result = db
            .run(|txn, _| async move {
                let r = rr_ref.read(&txn, Rank::from(3u64)).await.map_err(|e| {
                    foundationdb::FdbBindingError::CustomError(e.to_string().into())
                })?;
                Ok(r.into_value())
            })
            .await?;
        assert_eq!(read_result.as_deref(), Some(b"B".as_slice()));

        // Write rank 1 value "C" should be aborted (max_write_rank is 2)
        let rr_ref = &rr;
        let result = db
            .run(|txn, _| async move {
                let r = rr_ref
                    .write(&txn, Rank::from(1u64), b"C")
                    .await
                    .map_err(|e| {
                        foundationdb::FdbBindingError::CustomError(e.to_string().into())
                    })?;
                Ok(r)
            })
            .await?;
        assert_eq!(result, WriteResult::Aborted);

        Ok(())
    }

    async fn test_stale_write_rejected() -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let rr = setup_test(&db, "test_stale_write_rejected").await?;

        // Write with rank 5
        let rr_ref = &rr;
        let result = db
            .run(|txn, _| async move {
                let r = rr_ref
                    .write(&txn, Rank::from(5u64), b"initial")
                    .await
                    .map_err(|e| {
                        foundationdb::FdbBindingError::CustomError(e.to_string().into())
                    })?;
                Ok(r)
            })
            .await?;
        assert_eq!(result, WriteResult::Committed);

        // Read with rank 10 installs fence
        let rr_ref = &rr;
        db.run(|txn, _| async move {
            rr_ref
                .read(&txn, Rank::from(10u64))
                .await
                .map_err(|e| foundationdb::FdbBindingError::CustomError(e.to_string().into()))?;
            Ok(())
        })
        .await?;

        // Write with rank 7 should be aborted even though 7 > 5,
        // because max_read_rank is 10
        let rr_ref = &rr;
        let result = db
            .run(|txn, _| async move {
                let r = rr_ref
                    .write(&txn, Rank::from(7u64), b"stale")
                    .await
                    .map_err(|e| {
                        foundationdb::FdbBindingError::CustomError(e.to_string().into())
                    })?;
                Ok(r)
            })
            .await?;
        assert_eq!(result, WriteResult::Aborted);

        Ok(())
    }

    async fn test_follower_value_read() -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let rr = setup_test(&db, "test_follower_value_read").await?;

        // Write with rank 5
        let rr_ref = &rr;
        db.run(|txn, _| async move {
            rr_ref
                .write(&txn, Rank::from(5u64), b"X")
                .await
                .map_err(|e| foundationdb::FdbBindingError::CustomError(e.to_string().into()))?;
            Ok(())
        })
        .await?;

        // value() returns "X"
        let rr_ref = &rr;
        let val = db
            .run(|txn, _| async move {
                let v = rr_ref.value(&txn).await.map_err(|e| {
                    foundationdb::FdbBindingError::CustomError(e.to_string().into())
                })?;
                Ok(v)
            })
            .await?;
        assert_eq!(val.as_deref(), Some(b"X".as_slice()));

        // Write with rank 10 should succeed (value() didn't install a fence)
        let rr_ref = &rr;
        let result = db
            .run(|txn, _| async move {
                let r = rr_ref
                    .write(&txn, Rank::from(10u64), b"Y")
                    .await
                    .map_err(|e| {
                        foundationdb::FdbBindingError::CustomError(e.to_string().into())
                    })?;
                Ok(r)
            })
            .await?;
        assert_eq!(result, WriteResult::Committed);

        Ok(())
    }

    async fn test_empty_register() -> Result<(), FdbBindingError> {
        let db = crate::common::database().await?;
        let rr = setup_test(&db, "test_empty_register").await?;

        // Read on fresh register
        let rr_ref = &rr;
        let read_result = db
            .run(|txn, _| async move {
                let r = rr_ref.read(&txn, Rank::from(1u64)).await.map_err(|e| {
                    foundationdb::FdbBindingError::CustomError(e.to_string().into())
                })?;
                Ok((r.write_rank(), r.into_value()))
            })
            .await?;
        assert_eq!(read_result.0, Rank::ZERO);
        assert_eq!(read_result.1, None);

        // value() on fresh register
        let rr_ref = &rr;
        let val = db
            .run(|txn, _| async move {
                let v = rr_ref.value(&txn).await.map_err(|e| {
                    foundationdb::FdbBindingError::CustomError(e.to_string().into())
                })?;
                Ok(v)
            })
            .await?;
        assert_eq!(val, None);

        // Write with rank 1 should succeed
        let rr_ref = &rr;
        let result = db
            .run(|txn, _| async move {
                let r = rr_ref
                    .write(&txn, Rank::from(1u64), b"first")
                    .await
                    .map_err(|e| {
                        foundationdb::FdbBindingError::CustomError(e.to_string().into())
                    })?;
                Ok(r)
            })
            .await?;
        assert_eq!(result, WriteResult::Committed);

        Ok(())
    }
}
