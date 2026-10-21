cmake_minimum_required(VERSION 3.20)
project(pvd_repx_shared_shapes LANGUAGES CXX)
add_subdirectory("../physx-features" "feature-harness" EXCLUDE_FROM_ALL)
add_feature_producer(pvd-repx-shared-shapes main.cpp)
