let
  mkAudio =
    {
      level ? if language == null then "WithOther" else "All",
      language ? null,
    }:
    {
      inherit level;
      what = {
        Audio =
          if language == null then
            { }
          else
            {
              inherit language;
            };
      };
    };
in
{
  supported-formats = [
    "mkv"
  ];
  supported-codecs = [
    "HEVC"
    "H261"
    "H264"
    "VC1"
    "MPEG1VIDEO"
    "MPEG2VIDEO"
    "AAC"
  ];

  required = [
    {
      what = "Video";
      level = "All";
    }
    (mkAudio { language = "rus"; })
    (mkAudio {
      language = "eng";
      level = "AtLeastOne";
    })
    (mkAudio {})
  ];
}
