if [ `uname` = "Darwin" ]
then
    SED=gsed
else
    SED=sed
fi

function build_change {
    FILE=$1
    $SED -i '/.*# BUILD_BINDINGS_COMMENT/s/^/# /g' $FILE
    $SED -i '/.*# BUILD_BINDINGS_UNCOMMENT/s/^# //g' $FILE
}

cp python/Cargo.toml python/.Cargo.toml.bak
cp nlprule/Cargo.toml nlprule/.Cargo.toml.bak
cp build/Cargo.toml build/.Cargo.toml.bak
cp Cargo.toml .Cargo.toml.bak

build_change python/Cargo.toml
build_change nlprule/Cargo.toml
build_change build/Cargo.toml
build_change Cargo.toml

cd python
maturin $@
exit_code=$?
cd ..

mv python/.Cargo.toml.bak python/Cargo.toml
mv nlprule/.Cargo.toml.bak nlprule/Cargo.toml
mv build/.Cargo.toml.bak build/Cargo.toml
mv .Cargo.toml.bak Cargo.toml

exit $exit_code