if [ `uname` = "Darwin" ]
then
    SED=gsed
else
    SED=sed
fi

function set_cargo_toml_version {
    VERSION=$1
    FILE=$2

    $SED -i "0,/^version *= *\".*\"$/s//version = \"$VERSION\"/" $FILE
    $SED -i "s/^nlprule *=\(.*\)version *= *\".*\"\(.*\)/nlprule =\1version = \"$VERSION\"\2/" $FILE
}

set_cargo_toml_version $1 build/Cargo.toml
set_cargo_toml_version $1 nlprule/Cargo.toml
set_cargo_toml_version $1 python/Cargo.toml