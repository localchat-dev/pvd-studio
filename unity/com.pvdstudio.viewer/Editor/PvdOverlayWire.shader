Shader "Hidden/PVD Studio/Overlay Wire"
{
    Properties { _Color ("Line color", Color) = (0.42, 0.81, 0.93, 1) }
    SubShader
    {
        Tags { "Queue"="Transparent+10" "RenderType"="Transparent" }
        Pass
        {
            Cull Off
            ZTest LEqual
            ZWrite Off
            Offset -1, -1
            Blend SrcAlpha OneMinusSrcAlpha, Zero One

            CGPROGRAM
            #pragma target 4.0
            #pragma vertex vert
            #pragma geometry geom
            #pragma fragment frag
            #pragma multi_compile_instancing
            #include "UnityCG.cginc"

            half4 _Color;
            struct Attributes
            {
                float4 vertex : POSITION;
                UNITY_VERTEX_INPUT_INSTANCE_ID
            };
            struct Endpoint
            {
                float4 position : SV_POSITION;
                UNITY_VERTEX_OUTPUT_STEREO
            };
            struct Ribbon
            {
                float4 position : SV_POSITION;
                noperspective float across : TEXCOORD0;
                UNITY_VERTEX_OUTPUT_STEREO
            };

            Endpoint vert(Attributes input)
            {
                UNITY_SETUP_INSTANCE_ID(input);
                Endpoint output;
                UNITY_INITIALIZE_VERTEX_OUTPUT_STEREO(output);
                output.position = UnityObjectToClipPos(input.vertex);
                return output;
            }

            float nearDistance(float4 clip)
            {
                #if defined(UNITY_REVERSED_Z)
                    return clip.w - clip.z;
                #else
                    return clip.z - UNITY_NEAR_CLIP_VALUE * clip.w;
                #endif
            }

            [maxvertexcount(4)]
            void geom(line Endpoint endpoints[2], inout TriangleStream<Ribbon> stream)
            {
                float4 a = endpoints[0].position, b = endpoints[1].position;
                float da = nearDistance(a), db = nearDistance(b);
                if (da < 0 && db < 0) return;
                if (da < 0) a = lerp(a, b, da / (da - db));
                else if (db < 0) b = lerp(b, a, db / (db - da));

                float2 toPixel = _ScreenParams.xy * 0.5;
                float2 delta = (b.xy / b.w - a.xy / a.w) * toPixel;
                float lengthSquared = dot(delta, delta);
                if (lengthSquared < 1e-8) return;
                float2 along = delta * rsqrt(lengthSquared);
                float2 perpendicular = float2(-along.y, along.x);
                const float radius = 1.55;
                Ribbon output;
                UNITY_TRANSFER_VERTEX_OUTPUT_STEREO(endpoints[0], output);
                [unroll]
                for (int i = 0; i < 4; ++i)
                {
                    bool start = i < 2;
                    output.across = (i % 2 == 0) ? -radius : radius;
                    float2 offset = perpendicular * output.across + along * (start ? -0.45 : 0.45);
                    output.position = start ? a : b;
                    output.position.xy += (offset / toPixel) * output.position.w;
                    stream.Append(output);
                }
            }

            half4 frag(Ribbon input) : SV_Target
            {
                float distance = abs(input.across);
                half coverage = 1.0h - smoothstep(0.60, 1.55, distance);
                half edge = smoothstep(0.46, 1.18, distance);
                return half4(_Color.rgb * lerp(1.0h, 0.18h, edge), _Color.a * coverage);
            }
            ENDCG
        }
    }
}
