(function() {var implementors = {};
implementors["bindingtester"] = [{"text":"impl <a class=\"trait\" href=\"https://doc.rust-lang.org/1.60.0/core/convert/trait.From.html\" title=\"trait core::convert::From\">From</a>&lt;<a class=\"struct\" href=\"foundationdb/error/struct.FdbError.html\" title=\"struct foundationdb::error::FdbError\">FdbError</a>&gt; for <a class=\"struct\" href=\"bindingtester/struct.StackResult.html\" title=\"struct bindingtester::StackResult\">StackResult</a>","synthetic":false,"types":["bindingtester::StackResult"]},{"text":"impl <a class=\"trait\" href=\"https://doc.rust-lang.org/1.60.0/core/convert/trait.From.html\" title=\"trait core::convert::From\">From</a>&lt;<a class=\"enum\" href=\"foundationdb/tuple/element/enum.Element.html\" title=\"enum foundationdb::tuple::element::Element\">Element</a>&lt;'static&gt;&gt; for <a class=\"struct\" href=\"bindingtester/struct.StackResult.html\" title=\"struct bindingtester::StackResult\">StackResult</a>","synthetic":false,"types":["bindingtester::StackResult"]},{"text":"impl <a class=\"trait\" href=\"https://doc.rust-lang.org/1.60.0/core/convert/trait.From.html\" title=\"trait core::convert::From\">From</a>&lt;<a class=\"enum\" href=\"https://doc.rust-lang.org/1.60.0/core/result/enum.Result.html\" title=\"enum core::result::Result\">Result</a>&lt;<a class=\"enum\" href=\"foundationdb/tuple/element/enum.Element.html\" title=\"enum foundationdb::tuple::element::Element\">Element</a>&lt;'static&gt;, <a class=\"struct\" href=\"foundationdb/error/struct.FdbError.html\" title=\"struct foundationdb::error::FdbError\">FdbError</a>&gt;&gt; for <a class=\"struct\" href=\"bindingtester/struct.StackResult.html\" title=\"struct bindingtester::StackResult\">StackResult</a>","synthetic":false,"types":["bindingtester::StackResult"]}];
implementors["foundationdb"] = [{"text":"impl <a class=\"trait\" href=\"https://doc.rust-lang.org/1.60.0/core/convert/trait.From.html\" title=\"trait core::convert::From\">From</a>&lt;<a class=\"struct\" href=\"foundationdb/struct.FdbError.html\" title=\"struct foundationdb::FdbError\">FdbError</a>&gt; for <a class=\"enum\" href=\"foundationdb/directory/enum.DirectoryError.html\" title=\"enum foundationdb::directory::DirectoryError\">DirectoryError</a>","synthetic":false,"types":["foundationdb::directory::error::DirectoryError"]},{"text":"impl <a class=\"trait\" href=\"https://doc.rust-lang.org/1.60.0/core/convert/trait.From.html\" title=\"trait core::convert::From\">From</a>&lt;<a class=\"enum\" href=\"foundationdb/tuple/hca/enum.HcaError.html\" title=\"enum foundationdb::tuple::hca::HcaError\">HcaError</a>&gt; for <a class=\"enum\" href=\"foundationdb/directory/enum.DirectoryError.html\" title=\"enum foundationdb::directory::DirectoryError\">DirectoryError</a>","synthetic":false,"types":["foundationdb::directory::error::DirectoryError"]},{"text":"impl <a class=\"trait\" href=\"https://doc.rust-lang.org/1.60.0/core/convert/trait.From.html\" title=\"trait core::convert::From\">From</a>&lt;<a class=\"enum\" href=\"foundationdb/tuple/enum.PackError.html\" title=\"enum foundationdb::tuple::PackError\">PackError</a>&gt; for <a class=\"enum\" href=\"foundationdb/directory/enum.DirectoryError.html\" title=\"enum foundationdb::directory::DirectoryError\">DirectoryError</a>","synthetic":false,"types":["foundationdb::directory::error::DirectoryError"]},{"text":"impl <a class=\"trait\" href=\"https://doc.rust-lang.org/1.60.0/core/convert/trait.From.html\" title=\"trait core::convert::From\">From</a>&lt;<a class=\"struct\" href=\"foundationdb/struct.TransactionCommitted.html\" title=\"struct foundationdb::TransactionCommitted\">TransactionCommitted</a>&gt; for <a class=\"struct\" href=\"foundationdb/struct.Transaction.html\" title=\"struct foundationdb::Transaction\">Transaction</a>","synthetic":false,"types":["foundationdb::transaction::Transaction"]},{"text":"impl <a class=\"trait\" href=\"https://doc.rust-lang.org/1.60.0/core/convert/trait.From.html\" title=\"trait core::convert::From\">From</a>&lt;<a class=\"struct\" href=\"foundationdb/struct.TransactionCommitError.html\" title=\"struct foundationdb::TransactionCommitError\">TransactionCommitError</a>&gt; for <a class=\"struct\" href=\"foundationdb/struct.FdbError.html\" title=\"struct foundationdb::FdbError\">FdbError</a>","synthetic":false,"types":["foundationdb::error::FdbError"]},{"text":"impl <a class=\"trait\" href=\"https://doc.rust-lang.org/1.60.0/core/convert/trait.From.html\" title=\"trait core::convert::From\">From</a>&lt;<a class=\"struct\" href=\"foundationdb/struct.TransactionCancelled.html\" title=\"struct foundationdb::TransactionCancelled\">TransactionCancelled</a>&gt; for <a class=\"struct\" href=\"foundationdb/struct.Transaction.html\" title=\"struct foundationdb::Transaction\">Transaction</a>","synthetic":false,"types":["foundationdb::transaction::Transaction"]},{"text":"impl&lt;'a&gt; <a class=\"trait\" href=\"https://doc.rust-lang.org/1.60.0/core/convert/trait.From.html\" title=\"trait core::convert::From\">From</a>&lt;<a class=\"primitive\" href=\"https://doc.rust-lang.org/1.60.0/std/primitive.tuple.html\">(</a><a class=\"struct\" href=\"foundationdb/struct.KeySelector.html\" title=\"struct foundationdb::KeySelector\">KeySelector</a>&lt;'a&gt;, <a class=\"struct\" href=\"foundationdb/struct.KeySelector.html\" title=\"struct foundationdb::KeySelector\">KeySelector</a>&lt;'a&gt;<a class=\"primitive\" href=\"https://doc.rust-lang.org/1.60.0/std/primitive.tuple.html\">)</a>&gt; for <a class=\"struct\" href=\"foundationdb/struct.RangeOption.html\" title=\"struct foundationdb::RangeOption\">RangeOption</a>&lt;'a&gt;","synthetic":false,"types":["foundationdb::transaction::RangeOption"]},{"text":"impl <a class=\"trait\" href=\"https://doc.rust-lang.org/1.60.0/core/convert/trait.From.html\" title=\"trait core::convert::From\">From</a>&lt;<a class=\"primitive\" href=\"https://doc.rust-lang.org/1.60.0/std/primitive.tuple.html\">(</a><a class=\"struct\" href=\"https://doc.rust-lang.org/1.60.0/alloc/vec/struct.Vec.html\" title=\"struct alloc::vec::Vec\">Vec</a>&lt;<a class=\"primitive\" href=\"https://doc.rust-lang.org/1.60.0/std/primitive.u8.html\">u8</a>, <a class=\"struct\" href=\"https://doc.rust-lang.org/1.60.0/alloc/alloc/struct.Global.html\" title=\"struct alloc::alloc::Global\">Global</a>&gt;, <a class=\"struct\" href=\"https://doc.rust-lang.org/1.60.0/alloc/vec/struct.Vec.html\" title=\"struct alloc::vec::Vec\">Vec</a>&lt;<a class=\"primitive\" href=\"https://doc.rust-lang.org/1.60.0/std/primitive.u8.html\">u8</a>, <a class=\"struct\" href=\"https://doc.rust-lang.org/1.60.0/alloc/alloc/struct.Global.html\" title=\"struct alloc::alloc::Global\">Global</a>&gt;<a class=\"primitive\" href=\"https://doc.rust-lang.org/1.60.0/std/primitive.tuple.html\">)</a>&gt; for <a class=\"struct\" href=\"foundationdb/struct.RangeOption.html\" title=\"struct foundationdb::RangeOption\">RangeOption</a>&lt;'static&gt;","synthetic":false,"types":["foundationdb::transaction::RangeOption"]},{"text":"impl&lt;'a&gt; <a class=\"trait\" href=\"https://doc.rust-lang.org/1.60.0/core/convert/trait.From.html\" title=\"trait core::convert::From\">From</a>&lt;<a class=\"primitive\" href=\"https://doc.rust-lang.org/1.60.0/std/primitive.tuple.html\">(</a><a class=\"primitive\" href=\"https://doc.rust-lang.org/1.60.0/std/primitive.slice.html\">&amp;'a [</a><a class=\"primitive\" href=\"https://doc.rust-lang.org/1.60.0/std/primitive.u8.html\">u8</a><a class=\"primitive\" href=\"https://doc.rust-lang.org/1.60.0/std/primitive.slice.html\">]</a>, <a class=\"primitive\" href=\"https://doc.rust-lang.org/1.60.0/std/primitive.slice.html\">&amp;'a [</a><a class=\"primitive\" href=\"https://doc.rust-lang.org/1.60.0/std/primitive.u8.html\">u8</a><a class=\"primitive\" href=\"https://doc.rust-lang.org/1.60.0/std/primitive.slice.html\">]</a><a class=\"primitive\" href=\"https://doc.rust-lang.org/1.60.0/std/primitive.tuple.html\">)</a>&gt; for <a class=\"struct\" href=\"foundationdb/struct.RangeOption.html\" title=\"struct foundationdb::RangeOption\">RangeOption</a>&lt;'a&gt;","synthetic":false,"types":["foundationdb::transaction::RangeOption"]},{"text":"impl&lt;'a&gt; <a class=\"trait\" href=\"https://doc.rust-lang.org/1.60.0/core/convert/trait.From.html\" title=\"trait core::convert::From\">From</a>&lt;<a class=\"struct\" href=\"https://doc.rust-lang.org/1.60.0/core/ops/range/struct.Range.html\" title=\"struct core::ops::range::Range\">Range</a>&lt;<a class=\"struct\" href=\"foundationdb/struct.KeySelector.html\" title=\"struct foundationdb::KeySelector\">KeySelector</a>&lt;'a&gt;&gt;&gt; for <a class=\"struct\" href=\"foundationdb/struct.RangeOption.html\" title=\"struct foundationdb::RangeOption\">RangeOption</a>&lt;'a&gt;","synthetic":false,"types":["foundationdb::transaction::RangeOption"]},{"text":"impl&lt;'a&gt; <a class=\"trait\" href=\"https://doc.rust-lang.org/1.60.0/core/convert/trait.From.html\" title=\"trait core::convert::From\">From</a>&lt;<a class=\"struct\" href=\"https://doc.rust-lang.org/1.60.0/core/ops/range/struct.Range.html\" title=\"struct core::ops::range::Range\">Range</a>&lt;<a class=\"primitive\" href=\"https://doc.rust-lang.org/1.60.0/std/primitive.slice.html\">&amp;'a [</a><a class=\"primitive\" href=\"https://doc.rust-lang.org/1.60.0/std/primitive.u8.html\">u8</a><a class=\"primitive\" href=\"https://doc.rust-lang.org/1.60.0/std/primitive.slice.html\">]</a>&gt;&gt; for <a class=\"struct\" href=\"foundationdb/struct.RangeOption.html\" title=\"struct foundationdb::RangeOption\">RangeOption</a>&lt;'a&gt;","synthetic":false,"types":["foundationdb::transaction::RangeOption"]},{"text":"impl <a class=\"trait\" href=\"https://doc.rust-lang.org/1.60.0/core/convert/trait.From.html\" title=\"trait core::convert::From\">From</a>&lt;<a class=\"struct\" href=\"https://doc.rust-lang.org/1.60.0/core/ops/range/struct.Range.html\" title=\"struct core::ops::range::Range\">Range</a>&lt;<a class=\"struct\" href=\"https://doc.rust-lang.org/1.60.0/alloc/vec/struct.Vec.html\" title=\"struct alloc::vec::Vec\">Vec</a>&lt;<a class=\"primitive\" href=\"https://doc.rust-lang.org/1.60.0/std/primitive.u8.html\">u8</a>, <a class=\"struct\" href=\"https://doc.rust-lang.org/1.60.0/alloc/alloc/struct.Global.html\" title=\"struct alloc::alloc::Global\">Global</a>&gt;&gt;&gt; for <a class=\"struct\" href=\"foundationdb/struct.RangeOption.html\" title=\"struct foundationdb::RangeOption\">RangeOption</a>&lt;'static&gt;","synthetic":false,"types":["foundationdb::transaction::RangeOption"]},{"text":"impl&lt;'a&gt; <a class=\"trait\" href=\"https://doc.rust-lang.org/1.60.0/core/convert/trait.From.html\" title=\"trait core::convert::From\">From</a>&lt;<a class=\"struct\" href=\"https://doc.rust-lang.org/1.60.0/core/ops/range/struct.RangeInclusive.html\" title=\"struct core::ops::range::RangeInclusive\">RangeInclusive</a>&lt;<a class=\"primitive\" href=\"https://doc.rust-lang.org/1.60.0/std/primitive.slice.html\">&amp;'a [</a><a class=\"primitive\" href=\"https://doc.rust-lang.org/1.60.0/std/primitive.u8.html\">u8</a><a class=\"primitive\" href=\"https://doc.rust-lang.org/1.60.0/std/primitive.slice.html\">]</a>&gt;&gt; for <a class=\"struct\" href=\"foundationdb/struct.RangeOption.html\" title=\"struct foundationdb::RangeOption\">RangeOption</a>&lt;'a&gt;","synthetic":false,"types":["foundationdb::transaction::RangeOption"]},{"text":"impl <a class=\"trait\" href=\"https://doc.rust-lang.org/1.60.0/core/convert/trait.From.html\" title=\"trait core::convert::From\">From</a>&lt;<a class=\"struct\" href=\"https://doc.rust-lang.org/1.60.0/core/ops/range/struct.RangeInclusive.html\" title=\"struct core::ops::range::RangeInclusive\">RangeInclusive</a>&lt;<a class=\"struct\" href=\"https://doc.rust-lang.org/1.60.0/alloc/vec/struct.Vec.html\" title=\"struct alloc::vec::Vec\">Vec</a>&lt;<a class=\"primitive\" href=\"https://doc.rust-lang.org/1.60.0/std/primitive.u8.html\">u8</a>, <a class=\"struct\" href=\"https://doc.rust-lang.org/1.60.0/alloc/alloc/struct.Global.html\" title=\"struct alloc::alloc::Global\">Global</a>&gt;&gt;&gt; for <a class=\"struct\" href=\"foundationdb/struct.RangeOption.html\" title=\"struct foundationdb::RangeOption\">RangeOption</a>&lt;'static&gt;","synthetic":false,"types":["foundationdb::transaction::RangeOption"]},{"text":"impl <a class=\"trait\" href=\"https://doc.rust-lang.org/1.60.0/core/convert/trait.From.html\" title=\"trait core::convert::From\">From</a>&lt;<a class=\"struct\" href=\"foundationdb/struct.FdbError.html\" title=\"struct foundationdb::FdbError\">FdbError</a>&gt; for <a class=\"enum\" href=\"foundationdb/tuple/hca/enum.HcaError.html\" title=\"enum foundationdb::tuple::hca::HcaError\">HcaError</a>","synthetic":false,"types":["foundationdb::tuple::hca::HcaError"]},{"text":"impl <a class=\"trait\" href=\"https://doc.rust-lang.org/1.60.0/core/convert/trait.From.html\" title=\"trait core::convert::From\">From</a>&lt;<a class=\"enum\" href=\"foundationdb/tuple/enum.PackError.html\" title=\"enum foundationdb::tuple::PackError\">PackError</a>&gt; for <a class=\"enum\" href=\"foundationdb/tuple/hca/enum.HcaError.html\" title=\"enum foundationdb::tuple::hca::HcaError\">HcaError</a>","synthetic":false,"types":["foundationdb::tuple::hca::HcaError"]},{"text":"impl&lt;T&gt; <a class=\"trait\" href=\"https://doc.rust-lang.org/1.60.0/core/convert/trait.From.html\" title=\"trait core::convert::From\">From</a>&lt;<a class=\"struct\" href=\"https://doc.rust-lang.org/1.60.0/std/sync/poison/struct.PoisonError.html\" title=\"struct std::sync::poison::PoisonError\">PoisonError</a>&lt;T&gt;&gt; for <a class=\"enum\" href=\"foundationdb/tuple/hca/enum.HcaError.html\" title=\"enum foundationdb::tuple::hca::HcaError\">HcaError</a>","synthetic":false,"types":["foundationdb::tuple::hca::HcaError"]},{"text":"impl <a class=\"trait\" href=\"https://doc.rust-lang.org/1.60.0/core/convert/trait.From.html\" title=\"trait core::convert::From\">From</a>&lt;<a class=\"struct\" href=\"https://rust-random.github.io/rand/rand_core/error/struct.Error.html\" title=\"struct rand_core::error::Error\">Error</a>&gt; for <a class=\"enum\" href=\"foundationdb/tuple/hca/enum.HcaError.html\" title=\"enum foundationdb::tuple::hca::HcaError\">HcaError</a>","synthetic":false,"types":["foundationdb::tuple::hca::HcaError"]},{"text":"impl&lt;E:&nbsp;<a class=\"trait\" href=\"foundationdb/tuple/trait.TuplePack.html\" title=\"trait foundationdb::tuple::TuplePack\">TuplePack</a>&gt; <a class=\"trait\" href=\"https://doc.rust-lang.org/1.60.0/core/convert/trait.From.html\" title=\"trait core::convert::From\">From</a>&lt;E&gt; for <a class=\"struct\" href=\"foundationdb/tuple/struct.Subspace.html\" title=\"struct foundationdb::tuple::Subspace\">Subspace</a>","synthetic":false,"types":["foundationdb::tuple::subspace::Subspace"]},{"text":"impl&lt;'a&gt; <a class=\"trait\" href=\"https://doc.rust-lang.org/1.60.0/core/convert/trait.From.html\" title=\"trait core::convert::From\">From</a>&lt;&amp;'a <a class=\"struct\" href=\"foundationdb/tuple/struct.Subspace.html\" title=\"struct foundationdb::tuple::Subspace\">Subspace</a>&gt; for <a class=\"struct\" href=\"foundationdb/struct.RangeOption.html\" title=\"struct foundationdb::RangeOption\">RangeOption</a>&lt;'static&gt;","synthetic":false,"types":["foundationdb::transaction::RangeOption"]},{"text":"impl <a class=\"trait\" href=\"https://doc.rust-lang.org/1.60.0/core/convert/trait.From.html\" title=\"trait core::convert::From\">From</a>&lt;<a class=\"primitive\" href=\"https://doc.rust-lang.org/1.60.0/std/primitive.array.html\">[</a><a class=\"primitive\" href=\"https://doc.rust-lang.org/1.60.0/std/primitive.u8.html\">u8</a><a class=\"primitive\" href=\"https://doc.rust-lang.org/1.60.0/std/primitive.array.html\">; 12]</a>&gt; for <a class=\"struct\" href=\"foundationdb/tuple/struct.Versionstamp.html\" title=\"struct foundationdb::tuple::Versionstamp\">Versionstamp</a>","synthetic":false,"types":["foundationdb::tuple::versionstamp::Versionstamp"]},{"text":"impl <a class=\"trait\" href=\"https://doc.rust-lang.org/1.60.0/core/convert/trait.From.html\" title=\"trait core::convert::From\">From</a>&lt;<a class=\"struct\" href=\"https://doc.rust-lang.org/1.60.0/std/io/error/struct.Error.html\" title=\"struct std::io::error::Error\">Error</a>&gt; for <a class=\"enum\" href=\"foundationdb/tuple/enum.PackError.html\" title=\"enum foundationdb::tuple::PackError\">PackError</a>","synthetic":false,"types":["foundationdb::tuple::PackError"]},{"text":"impl&lt;'a&gt; <a class=\"trait\" href=\"https://doc.rust-lang.org/1.60.0/core/convert/trait.From.html\" title=\"trait core::convert::From\">From</a>&lt;<a class=\"primitive\" href=\"https://doc.rust-lang.org/1.60.0/std/primitive.slice.html\">&amp;'a [</a><a class=\"primitive\" href=\"https://doc.rust-lang.org/1.60.0/std/primitive.u8.html\">u8</a><a class=\"primitive\" href=\"https://doc.rust-lang.org/1.60.0/std/primitive.slice.html\">]</a>&gt; for <a class=\"struct\" href=\"foundationdb/tuple/struct.Bytes.html\" title=\"struct foundationdb::tuple::Bytes\">Bytes</a>&lt;'a&gt;","synthetic":false,"types":["foundationdb::tuple::Bytes"]},{"text":"impl <a class=\"trait\" href=\"https://doc.rust-lang.org/1.60.0/core/convert/trait.From.html\" title=\"trait core::convert::From\">From</a>&lt;<a class=\"struct\" href=\"https://doc.rust-lang.org/1.60.0/alloc/vec/struct.Vec.html\" title=\"struct alloc::vec::Vec\">Vec</a>&lt;<a class=\"primitive\" href=\"https://doc.rust-lang.org/1.60.0/std/primitive.u8.html\">u8</a>, <a class=\"struct\" href=\"https://doc.rust-lang.org/1.60.0/alloc/alloc/struct.Global.html\" title=\"struct alloc::alloc::Global\">Global</a>&gt;&gt; for <a class=\"struct\" href=\"foundationdb/tuple/struct.Bytes.html\" title=\"struct foundationdb::tuple::Bytes\">Bytes</a>&lt;'static&gt;","synthetic":false,"types":["foundationdb::tuple::Bytes"]},{"text":"impl&lt;'a&gt; <a class=\"trait\" href=\"https://doc.rust-lang.org/1.60.0/core/convert/trait.From.html\" title=\"trait core::convert::From\">From</a>&lt;&amp;'a <a class=\"primitive\" href=\"https://doc.rust-lang.org/1.60.0/std/primitive.str.html\">str</a>&gt; for <a class=\"struct\" href=\"foundationdb/tuple/struct.Bytes.html\" title=\"struct foundationdb::tuple::Bytes\">Bytes</a>&lt;'a&gt;","synthetic":false,"types":["foundationdb::tuple::Bytes"]},{"text":"impl <a class=\"trait\" href=\"https://doc.rust-lang.org/1.60.0/core/convert/trait.From.html\" title=\"trait core::convert::From\">From</a>&lt;<a class=\"struct\" href=\"https://doc.rust-lang.org/1.60.0/alloc/string/struct.String.html\" title=\"struct alloc::string::String\">String</a>&gt; for <a class=\"struct\" href=\"foundationdb/tuple/struct.Bytes.html\" title=\"struct foundationdb::tuple::Bytes\">Bytes</a>&lt;'static&gt;","synthetic":false,"types":["foundationdb::tuple::Bytes"]}];
if (window.register_implementors) {window.register_implementors(implementors);} else {window.pending_implementors = implementors;}})()