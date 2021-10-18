import socket
import cv2
import imagezmq
import simplejpeg    
from imutils.video import VideoStream

client_name = socket.gethostname()
vs = VideoStream(src=0).start()
jpeg_quality = 95 #Number between 0 and 100

sender = imagezmq.ImageSender(connect_to="tcp://ip_adress:portNo")

while True:
    frame = vs.read()
    frame_jpg = simplejpeg.encode_jpeg(frame, quality=jpeg_quality, colorspace="BGR")
    sender.send_jpg(client_name, frame_jpg)

