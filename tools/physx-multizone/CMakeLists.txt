cmake_minimum_required(VERSION 3.20)
project(pvd_physx_multizone LANGUAGES CXX)

if(NOT WIN32 OR NOT MSVC)
  message(FATAL_ERROR "The producer requires the existing Windows MSVC toolchain.")
endif()
set(PHYSX34_ROOT "" CACHE PATH "Pinned official PhysX 3.4 checkout")
set(PVD_STUDIO_ROOT "" CACHE PATH "pvd-studio checkout")
if(NOT EXISTS "${PVD_STUDIO_ROOT}/tools/physx-features/feature_harness.h")
  message(FATAL_ERROR "Set PVD_STUDIO_ROOT to the pvd-studio checkout.")
endif()
if(NOT EXISTS "${PHYSX34_ROOT}/PhysX_3.4/Include/PxPhysicsAPI.h")
  message(FATAL_ERROR "Set PHYSX34_ROOT to the pinned, built SDK checkout.")
endif()

add_executable(pvd-physx-multizone main.cpp)
target_compile_features(pvd-physx-multizone PRIVATE cxx_std_14)
set_property(TARGET pvd-physx-multizone PROPERTY MSVC_RUNTIME_LIBRARY MultiThreaded)
target_compile_definitions(pvd-physx-multizone PRIVATE NDEBUG PX_CHECKED=1)
target_include_directories(pvd-physx-multizone PRIVATE
  "${PHYSX34_ROOT}/PhysX_3.4/Include"
  "${PHYSX34_ROOT}/PxShared/include"
  "${PHYSX34_ROOT}/PxShared/src/foundation/include"
  "${PHYSX34_ROOT}/PxShared/src/pvd/include"
  "${PHYSX34_ROOT}/PxShared/src/pvd/src"
  "${PVD_STUDIO_ROOT}/tools/physx-features"
  "${PVD_STUDIO_ROOT}/tools/physx-sample/compat")
target_link_libraries(pvd-physx-multizone PRIVATE
  "${PHYSX34_ROOT}/PhysX_3.4/Lib/vc15win64/PhysX3CHECKED_x64.lib"
  "${PHYSX34_ROOT}/PhysX_3.4/Lib/vc15win64/PhysX3CommonCHECKED_x64.lib"
  "${PHYSX34_ROOT}/PhysX_3.4/Lib/vc15win64/PhysX3CookingCHECKED_x64.lib"
  "${PHYSX34_ROOT}/PhysX_3.4/Lib/vc15win64/PhysX3ExtensionsCHECKED.lib"
  "${PHYSX34_ROOT}/PxShared/lib/vc15win64/PxFoundationCHECKED_x64.lib"
  "${PHYSX34_ROOT}/PxShared/lib/vc15win64/PxPvdSDKCHECKED_x64.lib")
foreach(runtime
    PhysX_3.4/Bin/vc15win64/PhysX3CHECKED_x64.dll
    PhysX_3.4/Bin/vc15win64/PhysX3CommonCHECKED_x64.dll
    PhysX_3.4/Bin/vc15win64/PhysX3CookingCHECKED_x64.dll
    PxShared/bin/vc15win64/PxFoundationCHECKED_x64.dll
    PxShared/bin/vc15win64/PxPvdSDKCHECKED_x64.dll
    Externals/nvToolsExt/1/bin/x64/nvToolsExt64_1.dll)
  add_custom_command(TARGET pvd-physx-multizone POST_BUILD
    COMMAND "${CMAKE_COMMAND}" -E copy_if_different
      "${PHYSX34_ROOT}/${runtime}" "$<TARGET_FILE_DIR:pvd-physx-multizone>")
endforeach()
