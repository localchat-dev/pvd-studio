cmake_minimum_required(VERSION 3.20)
project(pvd_physx_features LANGUAGES CXX)

if(NOT WIN32 OR NOT MSVC)
  message(FATAL_ERROR "Feature producers require the existing Windows MSVC toolchain.")
endif()

set(PHYSX34_ROOT "" CACHE PATH "Pinned official PhysX 3.4 checkout")
if(NOT EXISTS "${PHYSX34_ROOT}/PhysX_3.4/Include/PxPhysicsAPI.h")
  message(FATAL_ERROR "Set PHYSX34_ROOT to the built official PhysX 3.4 checkout.")
endif()

function(add_feature_producer target source)
  add_executable(${target} ${source})
  target_compile_features(${target} PRIVATE cxx_std_14)
  set_property(TARGET ${target} PROPERTY MSVC_RUNTIME_LIBRARY MultiThreaded)
  target_compile_definitions(${target} PRIVATE NDEBUG PX_CHECKED=1)
  target_include_directories(${target} PRIVATE
    "${PHYSX34_ROOT}/PhysX_3.4/Include"
    "${PHYSX34_ROOT}/PxShared/include"
    "${PHYSX34_ROOT}/PhysX_3.4/Source/Common/src"
    "${PHYSX34_ROOT}/PhysX_3.4/Source/PhysXExtensions/src")
  target_link_libraries(${target} PRIVATE
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
    add_custom_command(TARGET ${target} POST_BUILD
      COMMAND "${CMAKE_COMMAND}" -E copy_if_different
      "${PHYSX34_ROOT}/${runtime}" "$<TARGET_FILE_DIR:${target}>")
  endforeach()
endfunction()

foreach(feature heightfield query_shapes joint_limits profile_metrics cct large_scene)
  if(EXISTS "${CMAKE_CURRENT_SOURCE_DIR}/${feature}.cpp")
    string(REPLACE "_" "-" target_suffix "${feature}")
    add_feature_producer("pvd-${target_suffix}" "${feature}.cpp")
  endif()
endforeach()

if(TARGET pvd-cct)
  target_link_libraries(pvd-cct PRIVATE
    "${PHYSX34_ROOT}/PhysX_3.4/Lib/vc15win64/PhysX3CharacterKinematicCHECKED_x64.lib")
  add_custom_command(TARGET pvd-cct POST_BUILD
    COMMAND "${CMAKE_COMMAND}" -E copy_if_different
    "${PHYSX34_ROOT}/PhysX_3.4/Bin/vc15win64/PhysX3CharacterKinematicCHECKED_x64.dll"
    "$<TARGET_FILE_DIR:pvd-cct>")
endif()

if(TARGET pvd-joint-limits)
  target_include_directories(pvd-joint-limits PRIVATE
    "${PHYSX34_ROOT}/PxShared/src/foundation/include"
    "${PHYSX34_ROOT}/PxShared/src/pvd/include"
    "${PHYSX34_ROOT}/PhysX_3.4/Include/common"
    "${CMAKE_CURRENT_SOURCE_DIR}/../physx-sample/compat")
endif()
