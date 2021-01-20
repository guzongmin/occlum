#!/bin/bash

if [ $# != 4 ] ; then
echo "USAGE: $0 major_version minor_version patch_version path"
echo " e.g.: $0 1 2 3 ."
exit 1;
fi

OCCLUM_MAJOR_VERSION=$1
OCCLUM_MINOR_VERSION=$2
OCCLUM_PATCH_VERSION=$3

dir=$4
if [ ! -d $dir ];then
echo $dir is not dir
fi

echo "update version to $OCCLUM_MAJOR_VERSION.$OCCLUM_MINOR_VERSION.$OCCLUM_PATCH_VERSION"

cd $dir
cd src/libos/
sed -i '/^version =/c\version = "'$OCCLUM_MAJOR_VERSION'.'$OCCLUM_MINOR_VERSION'.'$OCCLUM_PATCH_VERSION'"' Cargo.toml
sed -i '/^name = "Occlum"/{n;d}' Cargo.lock
sed -i '/^name = "Occlum"/a\version = "'$OCCLUM_MAJOR_VERSION'.'$OCCLUM_MINOR_VERSION'.'$OCCLUM_PATCH_VERSION'"' Cargo.lock
cd ../../

cd src/exec/
sed -i '/^version =/c\version = "'$OCCLUM_MAJOR_VERSION'.'$OCCLUM_MINOR_VERSION'.'$OCCLUM_PATCH_VERSION'"' Cargo.toml
sed -i '/^name = "occlum_exec"/{n;d}' Cargo.lock
sed -i '/^name = "occlum_exec"/a\version = "'$OCCLUM_MAJOR_VERSION'.'$OCCLUM_MINOR_VERSION'.'$OCCLUM_PATCH_VERSION'"' Cargo.lock
cd ../../

cd src/pal/include/
cat>occlum_version.h<<EOF
#ifndef _OCCLUM_VERSION_H_
#define _OCCLUM_VERSION_H_

// Version = $OCCLUM_MAJOR_VERSION.$OCCLUM_MINOR_VERSION.$OCCLUM_PATCH_VERSION
#define OCCLUM_MAJOR_VERSION    $OCCLUM_MAJOR_VERSION
#define OCCLUM_MINOR_VERSION    $OCCLUM_MINOR_VERSION
#define OCCLUM_PATCH_VERSION    $OCCLUM_PATCH_VERSION

#define STRINGIZE_PRE(X) #X
#define STRINGIZE(X) STRINGIZE_PRE(X)

#define OCCLUM_VERSION_NUM_STR STRINGIZE(OCCLUM_MAJOR_VERSION) "." \\
                    STRINGIZE(OCCLUM_MAJOR_VERSION) "." STRINGIZE(OCCLUM_PATCH_VERSION)

#endif
EOF
