Shader "Hidden/PVD Studio/Overlay Surface"
{
    Properties { _Color ("Tint", Color) = (0.42, 0.81, 0.93, 0.16) }
    SubShader
    {
        Tags { "Queue"="Transparent" "RenderType"="Transparent" }
        Pass
        {
            Cull Back
            ZTest LEqual
            ZWrite Off
            Offset -1, -1
            Blend SrcAlpha OneMinusSrcAlpha, Zero One

            CGPROGRAM
            #pragma target 3.5
            #pragma vertex vert
            #pragma fragment frag
            #pragma multi_compile_instancing
            #include "UnityCG.cginc"

            half4 _Color;
            struct Attributes
            {
                float4 vertex : POSITION;
                float3 normal : NORMAL;
                UNITY_VERTEX_INPUT_INSTANCE_ID
            };
            struct Varyings
            {
                float4 position : SV_POSITION;
                float3 viewPosition : TEXCOORD0;
                half3 viewNormal : TEXCOORD1;
                UNITY_VERTEX_OUTPUT_STEREO
            };

            Varyings vert(Attributes input)
            {
                UNITY_SETUP_INSTANCE_ID(input);
                Varyings output;
                UNITY_INITIALIZE_VERTEX_OUTPUT_STEREO(output);
                output.position = UnityObjectToClipPos(input.vertex);
                output.viewPosition = UnityObjectToViewPos(input.vertex.xyz);
                output.viewNormal = mul((float3x3)UNITY_MATRIX_V, UnityObjectToWorldNormal(input.normal));
                return output;
            }

            half4 frag(Varyings input) : SV_Target
            {
                half3 normal = normalize(input.viewNormal);
                float3 viewDirection = unity_OrthoParams.w > 0.5 ? float3(0, 0, 1) : normalize(-input.viewPosition);
                half grazing = 1.0h - saturate(abs(dot(normal, viewDirection)));
                half rim = grazing * grazing;
                half light = saturate(dot(normal, half3(-0.35h, 0.60h, 0.72h)));
                half3 tint = _Color.rgb * (0.76h + 0.24h * light);
                half opacity = _Color.a + 0.65h * rim * _Color.a * (1.0h - _Color.a);
                return half4(lerp(tint, _Color.rgb, rim), opacity);
            }
            ENDCG
        }
    }
}
