cmake_minimum_required(VERSION 3.20)
project(pvd_physx_sample LANGUAGES CXX)

if(NOT WIN32 OR NOT MSVC)
  message(FATAL_ERROR "This producer build currently requires Windows and MSVC.")
endif()

set(PHYSX34_ROOT "" CACHE PATH "Official PhysX 3.4 checkout")
if(NOT EXISTS "${PHYSX34_ROOT}/PhysX_3.4/Include/PxPhysicsAPI.h")
  message(FATAL_ERROR "Set PHYSX34_ROOT to the pinned official PhysX 3.4 checkout.")
endif()

add_executable(pvd-physx-sample main.cpp)
target_compile_features(pvd-physx-sample PRIVATE cxx_std_14)
set_property(TARGET pvd-physx-sample PROPERTY MSVC_RUNTIME_LIBRARY MultiThreaded)
target_compile_definitions(pvd-physx-sample PRIVATE NDEBUG PX_CHECKED=1)
target_include_directories(pvd-physx-sample PRIVATE
  "${PHYSX34_ROOT}/PhysX_3.4/Include"
  "${PHYSX34_ROOT}/PxShared/include"
  "${PHYSX34_ROOT}/PxShared/src/foundation/include"
  "${PHYSX34_ROOT}/PxShared/src/pvd/include"
  "${CMAKE_CURRENT_SOURCE_DIR}/compat")
target_link_libraries(pvd-physx-sample PRIVATE
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
  add_custom_command(TARGET pvd-physx-sample POST_BUILD
    COMMAND "${CMAKE_COMMAND}" -E copy_if_different
      "${PHYSX34_ROOT}/${runtime}" "$<TARGET_FILE_DIR:pvd-physx-sample>")
endforeach()
